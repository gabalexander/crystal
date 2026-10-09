#!/usr/bin/env python3
# The source of fixture/wiki.json, a wiki about crystal written by hand at commit 22ee18c:
#     python3 assets/wiki/dev/fixture.py assets/wiki/fixture/wiki.json
# Its links name real files and lines at that commit; tests/wiki_web.rs checks the files are there.
import json, sys

def c(label, path, a=None, b=None):
    frag = f"#L{a}" + (f"-L{b}" if b else "") if a else ""
    return f"[`{label}`](code:{path}{frag})"

D = lambda src, cap: {"mermaid": src.strip("\n"), "caption": cap}

overview_summary = f"""\
crystal is a terminal workspace for running many coding agents at once. A background daemon owns every agent's terminal, so an agent keeps working when its window closes, and one TUI shows them all: a sidebar listing each project, worktree and session with what the agent in it is doing, and the selected session live beside it.

Agents report what they're doing through hooks crystal adds to them, or crystal reads it off their screens with a rules file per agent, so the sidebar can say which are working, which wait on you and which are done. Every command, {c('crystal send', 'src/drive.rs', 1)} and {c('crystal attach', 'src/attach.rs', 1)} among them, is a client of the daemon over a Unix socket, which is how one agent drives another.

On top of that sit tasks, work given to an agent that stays open until it runs `crystal done`; a memory of what each project's sessions learned, which agents are shown as they start and as they open a file; and an event log that scripts and plugins follow.

Read on: [Sessions and the daemon](#sessions-and-the-daemon), [Knowing what agents do](#knowing-what-agents-do), [The TUI](#the-tui), and [Tasks, memory and events](#tasks-memory-and-events).
"""

overview_diagram = D("""
flowchart TD
  user["User<br/>(a terminal)"] -->|keys, mouse| tui["TUI<br/>(src/tui/mod.rs)"]
  cli["crystal commands<br/>(src/main.rs)"] -->|one JSON line each| sock["Unix socket<br/>(src/socket.rs)"]
  tui -->|requests, attach| sock
  sock --> daemon["Daemon<br/>(src/daemon.rs)"]
  daemon -->|owns PTYs| sessions["Sessions<br/>(src/session.rs)"]
  sessions -->|run| agents["Agents<br/>(Claude Code, Codex, ...)"]
  agents -.->|crystal hook| daemon
  daemon -->|emit| bus["Event bus<br/>(src/event_log.rs)"]
  bus -.->|subscribe| tui
  bus -->|events hooks| plugins["Plugins<br/>(src/plugin_hooks.rs)"]
  daemon -->|state| db["SQLite<br/>(src/db.rs)"]
""", "crystal's daemon between its clients and the agents it runs")

sections = []

# ---- 1 ------------------------------------------------------------------------------------------------
sections.append({
  "id": "sessions-and-the-daemon",
  "title": "Sessions and the daemon",
  "summary_md": f"""\
Everything crystal runs belongs to one long-lived process, the daemon, started by {c('daemon::run', 'src/daemon.rs', 130, 275)}. It listens on a Unix socket, owns a PTY for every session, keeps each session's screen up to date, and writes down what it needs to start them all again in its SQLite database ({c('Db', 'src/db.rs', 323)}). The TUI and every command are clients: each connects, sends one request and reads one response, starting the daemon first if nobody is listening.

Because the daemon holds the terminals, closing the TUI, or the terminal it ran in, stops nothing. Upgrading crystal doesn't either: the daemon runs the new binary in its own process with `exec`, keeping its children and their descriptors, and the new crystal carries on from a state file the old one handed it.
""",
  "diagram": D("""
sequenceDiagram
  participant Client as crystal (client)
  participant Daemon as daemon
  participant Session as session (PTY)
  Client->>Daemon: Request, one JSON line
  Daemon->>Daemon: same version? (Daemon::serve)
  Daemon->>Session: spawn, write keys, read screen
  Session-->>Daemon: output, exit status
  Daemon-->>Client: Response, one JSON line
""", "A request's trip from a client through the daemon to a session"),
  "subsections": [
    {
      "id": "the-daemon-and-its-socket",
      "title": "The daemon and its socket",
      "files": ["src/daemon.rs", "src/client.rs", "src/protocol.rs", "src/socket.rs"],
      "diagram": D("""
flowchart TD
  conn["Connection<br/>(UnixListener::incoming)"] -->|admitted by the gate| serve["Daemon::serve<br/>(src/daemon.rs)"]
  serve -->|another version| refuse["Response::Error<br/>(version_mismatch)"]
  serve -->|Attach| attach["attach<br/>(streams the screen)"]
  serve -->|Subscribe| events["stream_events<br/>(the event bus)"]
  serve -->|Handover| hand["hand_over<br/>(exec the new crystal)"]
  serve -->|anything else| handle["Daemon::handle<br/>(one response)"]
""", "How the daemon routes a request"),
      "body_md": f"""\
A client finds the socket with {c('socket::chosen', 'src/socket.rs', 25)}: `-S` and `--server` on the command line, then `CRYSTAL_SOCKET`, then `CRYSTAL_SERVER`, and otherwise the default server's socket in {c('socket::dir', 'src/socket.rs', 69)}. It asks with {c('client::ask', 'src/client.rs', 31)}, which writes the request as one line of JSON and reads one line back; asked to, it first starts the daemon with {c('start_daemon', 'src/client.rs', 703)} and waits up to {c('START_TIMEOUT', 'src/client.rs', 26)} for it to listen.

The daemon gets ready before it listens. {c('daemon::run', 'src/daemon.rs', 130, 275)} leaves the client's terminal with `setsid`, opens the database (a daemon that can't keep its state doesn't start, rather than run with none and write over it), then binds the socket with {c('listen', 'src/daemon.rs', 283)}, which removes a socket left behind by a daemon that crashed but refuses one somebody still answers on. It starts the event bus, the plugins' hooks and the worktree hooks, puts the sessions written down back in their places, and then takes connections, each answered on a thread of its own by {c('Daemon::answer', 'src/daemon.rs', 383)}.

{c('Daemon::serve', 'src/daemon.rs', 394, 459)} reads the request with {c('protocol::recv_request', 'src/protocol.rs', 1943)} and checks its version first: a crystal of another version gets {c('version_mismatch', 'src/daemon.rs', 4844)}'s message telling the user to run `crystal restart-server`, except for a shutdown and a handover, which every version must understand. Most requests go to {c('Daemon::handle', 'src/daemon.rs', 2256)} and get one {c('Response', 'src/protocol.rs', 732)}; a few take the connection over and answer as they go:

| Request | What the connection becomes |
|---|---|
| `Attach` | the session's screen, then its output as it comes, and the viewer's keys back |
| `Subscribe` | the events the filter takes, from {c('Daemon::stream_events', 'src/daemon.rs', 3378)} |
| `TakeLayoutOrders` | a TUI taking layout commands from the command line |
| `WaitOutput` | a wait for a pattern on the session's screen |
| `Handover` | the daemon handing itself over to a new crystal |

Every message is a variant of {c('Request', 'src/protocol.rs', 25)} or {c('Response', 'src/protocol.rs', 732)}, and their JSON Schema, which a test keeps in step with the types, is [`docs/crystal-api.schema.json`](code:docs/crystal-api.schema.json).
""",
    },
    {
      "id": "a-sessions-terminal-and-screen",
      "title": "A session's terminal and screen",
      "files": ["src/session.rs", "src/vt.rs", "src/output_ring.rs", "src/viewer.rs"],
      "diagram": D("""
flowchart LR
  prog["Program<br/>(an agent or a shell)"] -->|writes| pty["PTY master<br/>(portable_pty)"]
  pty -->|16 KiB reads| pump["Term::pump<br/>(src/session.rs)"]
  pump --> screen["vt::Screen<br/>(src/vt.rs)"]
  pump --> ring["OutputRing<br/>(src/output_ring.rs)"]
  pump -->|the same bytes| viewers["Viewers<br/>(src/viewer.rs)"]
  screen -.->|answers its questions| pty
""", "Where a session's output goes"),
      "body_md": f"""\
{c('Session::spawn', 'src/session.rs', 466, 520)} opens a PTY at {c('UNSEEN_SIZE', 'src/session.rs', 64)}, 120 columns by 40 rows, which stays its size until a viewer sizes it. The program starts with its environment cleared and set to the one {c('env::for_session', 'src/env.rs', 62)} builds, which names the session in `CRYSTAL_SESSION` so its hooks can find it. The daemon drops its copy of the terminal's other end at once, so that the program's exit ends the output, and waits for the program by its pid, the same way a daemon it's handed over to does.

A thread runs {c('Term::pump', 'src/session.rs', 2747, 2766)} for each session, reading 16 KiB at a time through {c('handover::readers', 'src/handover.rs', 326)}, which a handover can stop mid-read. Each read goes through {c('Screen::take', 'src/session.rs', 2793)} under the screen's lock:

- the daemon's own {c('vt::Screen', 'src/vt.rs', 113)}, an `alacritty_terminal` grid with its history, takes it first; it's what `crystal read`, the rules that read agents' screens and a new viewer catching up all see;
- the {c('OutputRing', 'src/output_ring.rs', 25)} keeps the last {c('MOST', 'src/output_ring.rs', 15)} bytes with a mark each second, for `crystal read --since` ({c('OutputRing::since', 'src/output_ring.rs', 108)});
- every {c('Viewer', 'src/viewer.rs', 13)} gets the same bytes, so its own screen stays in step: `crystal attach` and the TUI's panes are both viewers.

A program asking its terminal something (where the cursor is, what colours it has) is answered from the daemon's screen, and the answer written back to the PTY, so a program asks no viewer and never gets two answers. The screen also counts bells and keeps the last text a program asked to copy (OSC 52), for a viewer to pass on.
""",
    },
    {
      "id": "handing-the-daemon-over",
      "title": "Handing the daemon over",
      "files": ["src/handover.rs", "src/daemon.rs", "src/client.rs"],
      "diagram": D("""
flowchart TD
  ask["crystal restart-server<br/>(src/client.rs)"] -->|Request::Handover| check["handover::check<br/>(reads its FORMAT?)"]
  check -->|no| cold["Restart cold<br/>(sessions from SQLite)"]
  check -->|yes| gate["Gate::close<br/>(new connections wait)"]
  gate --> save["save_sessions_and_runs<br/>(src/db.rs)"]
  save --> state["handover::State<br/>(an unlinked file)"]
  state -->|exec, same pid| next["daemon::run<br/>(the new crystal)"]
  next --> adopt["take_over<br/>(Session::adopt)"]
  state -.->|exec fails| cold
""", "What restart-server does to a running daemon"),
      "body_md": f"""\
`crystal restart-server` asks the running daemon to become the newly installed crystal: {c('restart_daemon', 'src/client.rs', 625)} sends `Request::Handover` with the new binary and the {c('FORMAT', 'src/handover.rs', 52)} it reads, from {c('hand_over', 'src/client.rs', 654)}. The daemon's side is {c('Daemon::hand_over', 'src/daemon.rs', 1054, 1092)}.

What can be refused is refused before anything changes: {c('handover::check', 'src/handover.rs', 140)} runs the new binary to ask which format it reads, and a different one sends the client back to restart the daemon cold, the way it always did. Otherwise the gate closes ({c('Gate::close', 'src/handover.rs', 391)}): connections that come in now wait for the next crystal, and requests already being answered get up to {c('HANDOVER_GRACE', 'src/daemon.rs', 121)} to finish.

{c('exec_handed_over', 'src/daemon.rs', 1096, 1168)} then gets everything ready:

1. the sessions and flow runs are written to the database first, with {c('save_sessions_and_runs', 'src/db.rs', 477)}, so whatever goes wrong from here, the next daemon starts them again;
2. helpers like the distiller are waited for, and every output reader is stopped;
3. each session gives up what carries it on with {c('Session::hand_over', 'src/session.rs', 2116)}: its PTY's descriptor, its screen and history, its agent's state;
4. all of it goes into a {c('State', 'src/handover.rs', 57)}, written by {c('handover::write', 'src/handover.rs', 154)} to a file that's unlinked as soon as it's made, so the sessions' environments are never left on disk;
5. the descriptors to keep are marked with {c('keep_across_exec', 'src/handover.rs', 183)}, and {c('handover::exec', 'src/handover.rs', 220)} runs the new crystal in this process.

The pid stays the same, so every program the daemon started is still its child and waiting for one still says how it ended; the listening socket never closes, so a client only waits. The new crystal's {c('daemon::run', 'src/daemon.rs', 130)} reads the state and calls {c('take_over', 'src/daemon.rs', 1174)}, which brings each session back with {c('Session::adopt', 'src/session.rs', 2179)} and answers the clients that asked. herdr hands its sockets to a new process instead; that keeps the old daemon alive until the new one has taken over, but its programs stop being the daemon's children, which is why crystal execs.
""",
    },
  ],
})

# ---- 2 ------------------------------------------------------------------------------------------------
sections.append({
  "id": "knowing-what-agents-do",
  "title": "Knowing what agents do",
  "summary_md": f"""\
The sidebar's mark for a session, working, waiting on you, done, comes from two places. Agents that take hooks report as they go: crystal launches Claude Code with hooks of its own that run `crystal hook claude`, which tells the daemon each time a turn starts, a tool runs, a permission is asked for or the turn ends. Everything else, and what hooks never say (a turn cut short with Esc), is read off the agent's screen by a rules file for that agent.

Going the other way, `crystal send` types into an agent the way a person would, and watches that it took the text.
""",
  "diagram": D("""
flowchart LR
  hooks["Hooks<br/>(src/hook.rs)"] -->|AgentEvent| state["Session status<br/>(src/session.rs)"]
  rules["Screen rules<br/>(src/agent_rules.rs)"] -->|Looks| state
  state --> sidebar["Sidebar mark<br/>(src/tui/status.rs)"]
  state --> notify["Notifications<br/>(src/notify.rs)"]
  send["crystal send<br/>(src/drive.rs)"] -->|typing| agent["Agent's PTY"]
""", "The two ways crystal learns what an agent is doing"),
  "subsections": [
    {
      "id": "hooks-tell-the-daemon",
      "title": "Hooks tell the daemon",
      "files": ["src/agents.rs", "src/hook.rs", "src/subagents.rs", "src/asking.rs"],
      "diagram": D("""
flowchart LR
  claude["Claude Code<br/>(hooks via --settings)"] -->|runs| hook["crystal hook claude<br/>(src/hook.rs)"]
  hook -->|stdin JSON| map["hook_event<br/>(src/agents.rs)"]
  map -->|request| daemon["Daemon<br/>(src/daemon.rs)"]
  daemon --> status["Status<br/>(working, asking, done)"]
  hook -.->|PreToolUse on a file| recall["Recall<br/>(src/recall.rs)"]
""", "A hook's trip from Claude Code to the session's status"),
      "body_md": f"""\
crystal adds its hooks to Claude Code with `--settings` as it builds the command line in {c('agents::argv', 'src/agents.rs', 135)}, so the user's settings files are never touched and their own hooks run alongside. It listens to the events in {c('CLAUDE_HOOK_EVENTS', 'src/agents.rs', 29, 38)}, each running the command {c('hook_command', 'src/agents.rs', 70)} writes: `crystal hook claude`.

{c('hook::run', 'src/hook.rs', 33)} must never get in the agent's way, so it prints nothing unless what it prints is meant as input, and gives up quietly on any error. It finds its session from `CRYSTAL_SESSION` (outside crystal there's nobody to tell), reads the event's JSON from standard input, and tells the daemon. {c('hook_event', 'src/agents.rs', 390, 424)} reads what each event means:

| Claude Code's event | What it tells crystal |
|---|---|
| `UserPromptSubmit` | a turn started |
| `PostToolUse` | a tool finished, so the agent is at work |
| `PermissionRequest`, a `permission_prompt` notification | the agent is asking the user |
| `Stop` | the turn ended |
| `SubagentStart`, `SubagentStop` | a subagent started or stopped |

A turn that ends with subagents still running isn't the agent done: {c('Subagents', 'src/subagents.rs', 47)} holds the turn open until they've stopped and the agent hasn't taken their work up within {c('DRAINED_FOR', 'src/subagents.rs', 38)}. What the turn's last message said decides whether a session with its task open needs the user: {c('asking::judge', 'src/asking.rs', 105)} reads it with no model, erring towards asking.

One hook works the other way. As Claude Code is about to read or edit a file with one of {c('CLAUDE_FILE_TOOLS', 'src/agents.rs', 43)}, its `PreToolUse` hook asks the daemon what the project's memory says about that file, and prints it as context for Claude to read (see [Project memory](#project-memory)).
""",
    },
    {
      "id": "reading-the-screen",
      "title": "Reading the screen",
      "files": ["src/agent_rules.rs", "src/agent_screen.rs", "agents/claude.toml"],
      "diagram": D("""
flowchart TD
  screen["Rows, title, progress<br/>(vt::Screen)"] --> read["agent_screen::read<br/>(src/agent_screen.rs)"]
  files["Rules files<br/>(agents/*.toml)"] -->|Registry::load| registry["Registry<br/>(src/agent_rules.rs)"]
  registry -->|for_program| read
  read -->|working, waiting, settled| watch["ScreenWatch<br/>(holds for two checks)"]
  watch -->|AgentEvent| status["Session status"]
""", "Reading an agent's state off its screen"),
      "body_md": f"""\
Agents draw tell-tale text while they work or wait: a spinner at the start of the terminal's title, "esc to interrupt" under the prompt, a question with numbered choices. That text changes between versions, so it isn't in the code: each agent has a rules file in `agents/`, adapted from herdr's detection rules, bundled into crystal by {c('agent_rules::bundled', 'src/agent_rules.rs', 907)}. A file of the user's own in the config's `agents` directory takes the place of the bundled one, or adds an agent; {c('Registry::load', 'src/agent_rules.rs', 779)} reads them, and a broken one is said in the log and passed over.

A rule says what the screen looks like when it matches (working, waiting or settled), how much it counts, where to look and what to look for. Claude Code's first rule, in {c('agents/claude.toml', 'agents/claude.toml', 7, 13)}, reads the spinner in the title:

```toml
[[rules]]
id = "osc_title_working"
looks = "working"
priority = 1100
region = "title"
regex = ['^[\\x{{2800}}-\\x{{28FF}}\\x{{25D0}}-\\x{{25D3}}] ']
```

{c('agent_screen::read', 'src/agent_screen.rs', 31)} finds the rules for the program in front ({c('Registry::for_program', 'src/agent_rules.rs', 850)}) and {c('AgentRules::read', 'src/agent_rules.rs', 581)} tries them by priority, giving one of the {c('Looks', 'src/agent_screen.rs', 18)}, or nothing when the screen says nothing either way, like a menu over the prompt. A {c('ScreenWatch', 'src/agent_screen.rs', 45)} counts a new look only once it has held for two checks in a row, so a screen caught halfway through a redraw doesn't flip the session's mark.

`crystal agent explain` shows a session's reading rule by rule, through {c('AgentRules::explain', 'src/agent_rules.rs', 593)}: which rules matched, which didn't and why.
""",
    },
    {
      "id": "typing-into-an-agent",
      "title": "Typing into an agent",
      "files": ["src/typing.rs", "src/drive.rs", "src/messages.rs"],
      "diagram": D("""
stateDiagram-v2
  [*] --> Typed: text pasted
  Typed --> Entered: Enter, after ENTER_PAUSE
  Entered --> Taken: a turn starts, or the screen changes
  Entered --> Entered: Enter again, up to ENTERS times
  Entered --> Stalled: never taken
  Taken --> [*]
  Stalled --> [*]
""", "How a send to an agent at its prompt goes"),
      "body_md": f"""\
A program can only tell typing from pasting by how fast the keys come, and agents like Claude Code take a burst of text ending in Enter for a paste: the Enter goes into the prompt instead of sending it. So {c('typing::keystrokes', 'src/typing.rs', 34)} marks the text as a bracketed paste when the program asked for that, and the Enter goes on its own, {c('ENTER_PAUSE', 'src/typing.rs', 23)} later.

An agent at its prompt doesn't always take what's typed: it may still be drawing, or a dialog may have opened over the prompt. A {c('Delivery', 'src/typing.rs', 142)} watches what happens after the Enter. A turn starting, or the screen changing, means the agent took the text; neither means Enter again, up to {c('ENTERS', 'src/typing.rs', 119)} times, and then the send is stalled, which `crystal send` exits 3 for, so a script can tell it from a timeout (2).

An agent crystal stopped for being idle is started again for a send to reach. {c('Waking', 'src/typing.rs', 81)} decides when it's ready to be typed into: at its prompt, taking keys, its screen held still.

What one session sends another goes through `src/messages.rs` first: the text tidied and cut to 8 KiB, a line ahead of it naming the session that sent it, at most 20 sends a minute, and a message that only acknowledges (`ok`, `thanks`) refused, so two agents can't thank each other forever.
""",
    },
  ],
})

# ---- 3 ------------------------------------------------------------------------------------------------
sections.append({
  "id": "the-tui",
  "title": "The TUI",
  "summary_md": f"""\
`crystal` with no command runs the TUI ({c('tui::run', 'src/tui/mod.rs', 409)}): a sidebar with every session, the selected one live in a pane beside it, and any others split off into panes of their own, in tabs that each keep their own. It's one event loop over one channel of events. The state and how keys change it live in {c('App', 'src/tui/app.rs', 1325)}, which does no I/O, so every key can be tested; drawing only reads the state.
""",
  "diagram": D("""
flowchart LR
  input["Input reader<br/>(crossterm)"] -->|Key, Mouse, Paste| chan["One channel<br/>(src/tui/mod.rs)"]
  poller["Session poller<br/>(asks the daemon)"] -->|Sessions| chan
  panes["Pane viewers<br/>(src/tui/pane.rs)"] -->|Output| chan
  chan --> app["App<br/>(src/tui/app.rs)"]
  app -->|Action| io["The loop's I/O<br/>(start, attach, kill)"]
  app --> draw["ui::draw<br/>(src/tui/ui.rs)"]
""", "The TUI's event loop"),
  "subsections": [
    {
      "id": "the-event-loop-and-the-state",
      "title": "The event loop and the state",
      "files": ["src/tui/mod.rs", "src/tui/app.rs", "src/tui/ui.rs"],
      "diagram": D("""
flowchart TD
  key["Key pressed<br/>(Event::Key)"] --> on_key["App::on_key<br/>(src/tui/app.rs)"]
  on_key -->|state only| state["App's state<br/>(tabs, selection, views)"]
  on_key -->|needs the world| action["Action<br/>(Start, Kill, Attach, ...)"]
  action --> loop["Event loop<br/>(src/tui/mod.rs)"]
  loop -->|request| daemon["Daemon"]
  state --> draw["ui::draw<br/>(reads, never writes)"]
""", "How a key becomes a change of state or an action"),
      "body_md": f"""\
{c('tui::run', 'src/tui/mod.rs', 409, 470)} refuses to start without a terminal, loads the config, and lists the sessions, which starts the daemon if it isn't running. Then it starts the threads that feed its one `mpsc` channel of {c('Event', 'src/tui/mod.rs', 176)}s: the input reader, the session poller, the forge poller for pull requests and issues, the worktree lister, the counter of each worktree's uncommitted changes, the resource poller, the config file's watcher, and a viewer for each pane.

The loop takes an event, updates the state and draws. A key goes to {c('App::on_key', 'src/tui/app.rs', 4092)}. Most keys only change the state: the selection, a view opened, a split resized. A key that needs the outside world comes back as an {c('Action', 'src/tui/app.rs', 431)} instead, such as starting a session somewhere, killing one or opening a file in the editor, and the event loop carries it out. Keeping I/O out of `App` is what lets its thousands of lines of state changes be unit-tested without a daemon or a terminal.

{c('ui::draw', 'src/tui/ui.rs', 489)} lays out the tab bar, the sidebar, the panes and the footer from the state, without boxes: thin rules and the theme's colours tell the parts apart. Each pane's {c('Pane', 'src/tui/pane.rs', 19)} is a viewer of its session, its own screen drawn with the screen widget, and copy mode over it while that's on.
""",
    },
    {
      "id": "tabs-and-split-panes",
      "title": "Tabs and split panes",
      "files": ["src/tui/tabs.rs", "src/tui/split_tree.rs"],
      "diagram": D("""
classDiagram
  class Tabs {
    tabs: Vec~Tab~
    current: usize
  }
  class Tab {
    sessions: Vec~String~
    selected: Option~String~
    panes: SplitTree
    floating: Option~String~
  }
  class SplitTree {
    layout(area)
    split(at, way, ratio, new)
    close(pane)
    resize(pane, toward, cells)
  }
  class Pane {
    Selection
    Session(name)
  }
  Tabs "1" --> "many" Tab
  Tab --> SplitTree
  SplitTree --> Pane
""", "Tabs, their split trees and panes"),
      "body_md": f"""\
Each {c('Tab', 'src/tui/tabs.rs', 29, 60)} is a space of its own: its sessions (every session is in exactly one tab), its selection, its tree of panes and the session floating over them. {c('Tabs', 'src/tui/tabs.rs', 160)} keeps them in order with the one in front. The sidebar shows only the front tab's sessions, so going to another tab changes what the sidebar lists. Tabs are kept in the database as a JSON document; {c('tabs::read', 'src/tui/tabs.rs', 450)} also reads the shape from before panes were a tree.

A tab's panes are a {c('SplitTree', 'src/tui/split_tree.rs', 99)}: each split cuts its room in two, side by side or one above the other ({c('Way', 'src/tui/split_tree.rs', 38)}), at a ratio, as deep as the user likes. One {c('Pane', 'src/tui/split_tree.rs', 26)} follows the sidebar's selection; each of the others keeps a session of its own.

- {c('SplitTree::layout', 'src/tui/split_tree.rs', 199)} gives each pane its rectangle for an area of the screen;
- {c('SplitTree::split', 'src/tui/split_tree.rs', 241)} and {c('SplitTree::close', 'src/tui/split_tree.rs', 256)} change the tree, a closed pane's sibling taking its room;
- {c('SplitTree::resize', 'src/tui/split_tree.rs', 344)} moves the nearest border on the way asked, within each pane's least size.

All of it is pure, so it's unit-tested, and folds into a value the database keeps.
""",
    },
    {
      "id": "the-sidebar",
      "title": "The sidebar",
      "files": ["src/tui/sidebar.rs", "src/tui/groups.rs", "src/tui/rows.rs", "src/tui/status.rs"],
      "diagram": D("""
flowchart TD
  list["SessionInfo list<br/>(from the daemon)"] --> order["groups::order<br/>(attention or stable, by hand)"]
  order --> rows["groups::rows<br/>(project, worktree, session)"]
  layout["sidebar rows setting<br/>(src/tui/rows.rs)"] --> draw["sidebar::draw<br/>(src/tui/sidebar.rs)"]
  rows --> draw
  draw --> marks["session_mark and ago<br/>(what it does, since when)"]
""", "From the daemon's list of sessions to the sidebar's rows"),
      "body_md": f"""\
The sidebar lists every session of the front tab under its project, then its worktree. {c('groups::order', 'src/tui/groups.rs', 190)} sorts them: agents before terminals in each worktree, then by attention (those needing the user first) or stable, as `[sidebar] order` says, then as the user moved projects and sessions by hand ({c('ByHand', 'src/tui/groups.rs', 108)}), then as they were made. {c('groups::rows', 'src/tui/groups.rs', 246)} turns that into {c('Row', 'src/tui/groups.rs', 31)}s: headings, worktree lines, sessions, each flow run's steps under it, and linked worktrees with nothing running at the end of their project.

{c('sidebar::draw', 'src/tui/sidebar.rs', 61)} draws them. Each session gets a mark for what it's doing from {c('session_mark', 'src/tui/sidebar.rs', 1438)} and how long ago that changed from {c('ago', 'src/tui/sidebar.rs', 1561)}; sessions that need the user are also pinned at the top, with the tab each is in. What goes on a session's lines is the user's to lay out: `[sidebar] rows` names tokens like the agent's model, the branch or the task, read and checked in `src/tui/rows.rs`, and whatever doesn't fit the width is left out rather than wrapped.
""",
    },
  ],
})

# ---- 4 ------------------------------------------------------------------------------------------------
sections.append({
  "id": "tasks-memory-and-events",
  "title": "Tasks, memory and events",
  "summary_md": f"""\
Three things are built on the sessions. A task is a session started with something to do, numbered like `t12`, which stays open until its agent runs `crystal done`. The memory keeps what each project's sessions learned, distilled from what a closed task did, and shows it to the agents that come after. And everything that happens is an event, numbered, written to a log, streamed to subscribers and handed to plugins.
""",
  "diagram": D("""
flowchart LR
  task["Task<br/>(src/tasks.rs)"] -->|crystal done| closed["Closed task<br/>(src/work.rs)"]
  closed -->|transcript| distill["Distiller<br/>(src/distill.rs)"]
  distill -->|lessons| memory["Memory<br/>(src/memory.rs)"]
  memory -.->|recall on a file| agent["The next agent"]
  closed -->|task.closed| bus["Event bus<br/>(src/event_log.rs)"]
  bus --> plugins["Plugins<br/>(src/plugin_hooks.rs)"]
""", "How a closed task feeds the memory and the event log"),
  "subsections": [
    {
      "id": "tasks-and-crystal-done",
      "title": "Tasks and crystal done",
      "files": ["src/tasks.rs", "src/work.rs", "src/task.rs", "src/claude_stream.rs"],
      "diagram": D("""
flowchart TD
  new["crystal tasks new<br/>(src/work.rs)"] -->|numbered t12| start["Session with a task<br/>(src/daemon.rs)"]
  start -->|told about crystal done| agent["Agent works"]
  agent -->|turn ends, task open| remind["REMINDER<br/>(src/tasks.rs)"]
  remind --> agent
  agent -->|crystal done| close["Request::Close<br/>(work::done)"]
  close --> history["Project history<br/>(src/db.rs)"]
""", "A task's life"),
      "body_md": f"""\
A task is told about closing itself as it starts: {c('tasks::instructions', 'src/tasks.rs', 48)} writes the paragraph its agent reads, and acceptance criteria given with `--accept` go under the goal ({c('with_criteria', 'src/tasks.rs', 121)}), at most {c('MAX_CRITERIA_BYTES', 'src/tasks.rs', 80)} of them. An agent that ends a turn with its task still open and says nothing clear is sent the {c('REMINDER', 'src/tasks.rs', 70)} through its `Stop` hook, once.

{c('work::done', 'src/work.rs', 31, 53)} is what the agent runs: it finds its own session, makes any `--artifact` paths absolute (the daemon reads them, wherever it runs) and sends `Request::Close`. The daemon closes the task, keeps the artifacts beside it, writes it into the project's history, tells `task.closed`, and has the distiller read what was done.

A background task is Claude Code with no terminal at all. {c('Task', 'src/task.rs', 65)} runs `claude -p` with the arguments in {c('claude_stream::ARGS', 'src/claude_stream.rs', 34)}, writing the prompt and every follow-up to its standard input and reading its stream-json events back ({c('claude_stream::read', 'src/claude_stream.rs', 145)}), which it draws as a transcript on the session's screen. A permission Claude asks for arrives as a {c('PermissionRequest', 'src/claude_stream.rs', 127)}, goes to the user like any other session's question, and their answer goes back as a control message ({c('claude_stream::answer', 'src/claude_stream.rs', 82)}).
""",
    },
    {
      "id": "project-memory",
      "title": "Project memory",
      "files": ["src/memory.rs", "src/distill.rs", "src/recall.rs", "src/mcp.rs"],
      "diagram": D("""
flowchart TD
  closed["Closed task<br/>(transcript)"] --> run["distill::run<br/>(claude -p, Haiku)"]
  run -->|at most MAX_DISTILLED| add["Store::add<br/>(src/memory.rs)"]
  add -->|same said again| seen["Seen again<br/>(no new entry)"]
  add -->|corrects an older one| retire["Store::retire / update"]
  add --> fts["FTS5 index + vectors"]
  fts -->|search| mcp["memory_search<br/>(src/mcp.rs)"]
  fts -->|about_file| recall["Recall<br/>(src/recall.rs)"]
""", "How lessons get into the memory and back out"),
      "body_md": f"""\
Every project's entries live in one SQLite database in crystal's state directory, the {c('Store', 'src/memory.rs', 964)}, with an FTS5 index ranked by bm25 so a search finds an entry by any of its words or a word they stem from; with search by meaning on, each entry also has a vector, and the two rankings are merged. Anyone can add: the user, an agent with `crystal remember`, and the distiller. {c('Store::add', 'src/memory.rs', 993)} never keeps the same thing twice: said again, in the same words or (with the models) in others, it's the one entry seen again.

The distiller runs after a task closes. {c('distill::run', 'src/distill.rs', 733)} hands a tool-less `claude -p` the end of the transcript and the entries nearest in meaning to what the session said, with {c('SYSTEM_PROMPT', 'src/distill.rs', 157)} telling it to keep lessons alone (decisions, gotchas, commands that work) and never progress or status. It keeps at most {c('MAX_DISTILLED', 'src/distill.rs', 62)} and gives up after {c('TIMEOUT', 'src/distill.rs', 69)}. What it keeps can correct what was kept before: an older entry is rewritten in place ({c('Store::update', 'src/memory.rs', 1244)}) or retired for the new one ({c('Store::retire', 'src/memory.rs', 1319)}), kept apart with why.

Agents get it back three ways: a paragraph of the most relevant entries as they start; the `memory_search` and `memory_show` tools of {c('crystal mcp', 'src/mcp.rs', 1)}; and recall, as Claude Code opens a file. Recall asks {c('Store::about_file', 'src/memory.rs', 2090)} for the few entries about that file the session hasn't been shown yet ({c('Recalled', 'src/recall.rs', 54)} remembers what it was shown), and waits at most {c('recall::WAIT', 'src/recall.rs', 47)} so a slow lookup never holds the agent up.
""",
    },
    {
      "id": "events-and-plugins",
      "title": "Events and plugins",
      "files": ["src/events.rs", "src/event_log.rs", "src/plugin_hooks.rs"],
      "diagram": D("""
flowchart LR
  daemon["Daemon<br/>(src/daemon.rs)"] -->|emit| bus["Bus::emit<br/>(src/event_log.rs)"]
  bus -->|seq, add_event| log["events table<br/>(src/db.rs)"]
  bus -->|filter matches| subs["Subscribers<br/>(TUI, crystal events)"]
  bus -->|its own thread| hooks["Plugin hooks<br/>(src/plugin_hooks.rs)"]
  hooks -->|stdin, CRYSTAL_EVENT_JSON| plugin["A plugin's command"]
  subs -.->|falls behind| dropped["Dropped"]
""", "Where an event goes"),
      "body_md": f"""\
Whatever happens to a session, a task, a flow, a worktree, the memory or the backlog is an {c('Event', 'src/events.rs', 378)} of some {c('Kind', 'src/events.rs', 32)}. The kinds are a public contract plugins listen for: a kind is added, never renamed, and a test keeps the table of them in `docs/plugins.md` in step with the code.

Every event goes through the daemon's one {c('Bus', 'src/event_log.rs', 49)}. {c('Bus::emit', 'src/event_log.rs', 113, 135)} numbers it with a `seq` that never goes back, stamps the time, writes it to the `events` table, and offers it to each subscriber whose filter takes it. A subscriber too slow to keep up, its {c('BACKLOG', 'src/event_log.rs', 35)} full, is dropped rather than allowed to hold the daemon up; it reconnects and asks for what it missed by `seq`. The log is pruned by age every {c('PRUNE_EVERY', 'src/event_log.rs', 31)} and kept to {c('MOST', 'src/event_log.rs', 28)} events.

Subscribers are the TUI, `crystal events --follow`, `crystal wait` and the plugins. {c('plugin_hooks::follow', 'src/plugin_hooks.rs', 48)} subscribes for them: each plugin's `[[events]]` hooks run one at a time on a thread of the plugin's own, so a slow plugin delays only itself, with the event as JSON on standard input, in `CRYSTAL_EVENT_JSON`, and in words in `CRYSTAL_EVENT_TEXT`. A plugin whose hooks fail {c('FAILURES_TO_PAUSE', 'src/plugin_hooks.rs', 42)} times in a row is paused, with a `plugin.paused` event saying so.
""",
    },
  ],
})

wiki = {
  "version": 1,
  "repo": {
    "name": "gabalexander/crystal",
    "root": "/Users/you/src/crystal",
    "commit": "22ee18c82b0956c41604769ec94cf6dcb6580ccd",
    "branch": "master",
    "web_url": "https://github.com/gabalexander/crystal",
    "code_url": "https://github.com/gabalexander/crystal/blob/{commit}/{path}",
  },
  "generated": {
    "at": "2026-10-09T12:00:00Z",
    "by": "Claude Sonnet 5.5",
    "model": "claude-sonnet-5-5",
    "cost_usd": 4.2,
    "crystal": "0.3.0",
  },
  "overview": {"summary_md": overview_summary, "diagram": overview_diagram},
  "sections": sections,
}
json.dump(wiki, open(sys.argv[1], "w"), indent=2, ensure_ascii=False)
open(sys.argv[1], "a").write("\n")
