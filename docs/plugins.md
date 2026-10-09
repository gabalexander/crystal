# Plugins

<sub>[← README](../README.md#documentation)</sub>

Most of what crystal does beyond running sessions is a plugin you can switch off: tasks, handoff notes, the
backlog, memory, profiles, GitHub and GitLab, flows, notifications and the wiki. Plugins of your own add actions, panes
over the TUI or among its panes, hooks on what happens, commands to run as the daemon starts and links to open
their own way, and use crystal through its own command line, like any script would. A project can ship its own
in its repository, which you turn on for it alone.

- [Switching them on and off](#switching-them-on-and-off)
- [Writing a plugin](#writing-a-plugin)
- [A project's own plugins](#a-projects-own-plugins)
- [Building](#building)
- [Startup](#startup)
- [Link handlers](#link-handlers)
- [Events](#events)
- [Examples](#examples)
- [Security](#security)

## Switching them on and off

```sh
crystal plugin                     # every plugin, the project's too, and whether it's on
crystal plugin disable github      # or enable; written under [plugins] in the config file
crystal plugin new notes           # a plugin to start from, in ~/.config/crystal/plugins/notes
crystal plugin install <git-url>   # or a directory; shows what it runs and asks first, then builds it
crystal plugin build notes         # run its build commands again
crystal plugin run notes hello     # run one of its actions
crystal plugin run notes --event session.waiting   # try its hooks on a made-up event
crystal events --json | tail -1 | crystal plugin run notes --json -   # or on a real one, or what you write
crystal plugin run notes --link https://…          # run what its link handlers do with a link
crystal plugin pane open notes board               # open one of its panes, where it says or --placement does
crystal plugin events              # every event its hooks can hear, and when
crystal plugin log notes           # what its commands printed, and how they failed
crystal plugin remove notes
crystal plugin enable lint --project               # a plugin the project ships, shown first, for it alone
```

| Plugin | What it adds |
|---|---|
| `tasks` | [tasks](tasks.md#tasks): `c`, a task under its session, `crystal done` and `tasks`, and telling agents how to close theirs |
| `handoff` | [the handoff file](tasks.md#the-handoff-file): `crystal handoff`, the note a closing task adds and the copy it keeps, and telling agents to read the notes |
| `backlog` | [the backlog](tasks.md#the-backlog): `b`, the counts beside projects, `crystal backlog`, and telling agents to use it |
| `memory` | [memory](memory.md): `m`, `crystal remember` and `memory`, and what Claude Code is shown as it starts |
| `profiles` | [profiles](configuration.md#profiles): `P`, the profiles in the new-session panel, and `crystal profile` |
| `github` | [pull requests and issues](code.md#pull-requests-and-issues), on GitHub or GitLab: their marks on worktree lines, `o`, `O` and `i`; switched off, crystal never runs `gh` or `glab` |
| `flows` | [flows](flows.md): `g` and `f`, runs in the sidebar and the new-session panel, and `crystal flow` |
| `notifications` | telling you when a session needs you, with a notification and a sound |
| `wiki` | [the wiki](wiki.md): a page about each project's code in the browser, `crystal wiki`, and its actions in `X`: build, update and open |

crystal's own plugins are on until you switch one off. Then everything it adds is gone: its keys (`?` stops
listing them), what it shows in the sidebar and the new-session panel, what it tells agents, and the work it
does in the background. Its commands still run, to say that it's off and how to turn it on. `X` in the TUI lists
every plugin, and `Space` switches the one the bar is on, straight away; `Enter` on one of its actions, like the
wiki's, runs it in the background, in the selected session's worktree, and says how it went on the footer. Either way it's written to the config
file, keeping your comments:

```toml
[plugins]
github = false
notes = true
```

The `notify` setting came before the `notifications` plugin and still works: notifications are on only while
both are. Sounds go with the plugin too, but not with `notify`: `[sound]` has a switch of its own. `memory` used to be a setting of its own; crystal says where it went if it finds one.

## Writing a plugin

A plugin is a directory in `~/.config/crystal/plugins/` (or `$XDG_CONFIG_HOME/crystal/plugins/`) named after
it, with a `plugin.toml`. `crystal plugin new <name>` makes one with one of everything to start from. A plugin
you add is off until you turn it on.

```toml
name = "notes"                # its directory's name: lowercase letters, digits and dashes
version = "0.1.0"
description = "Notes on sessions"
min_crystal_version = "0.3.0" # optional: the oldest crystal it works with
platforms = ["macos", "linux"] # optional: where it runs; anywhere, left out
timeout_secs = 60             # optional: how long a hook or startup command may run; 30, left out

[[build]]                     # run as it's installed, and by `crystal plugin build notes`
command = ["npm", "ci"]
platforms = ["linux"]         # optional, here and on a startup command: only there

[[startup]]                   # run by the daemon as it starts
command = ["sh", "restore.sh"]
timeout_secs = 120            # optional, here and on a hook: in place of the plugin's

[[actions]]                   # run from X, its key, or `crystal plugin run notes add`
id = "add"
title = "Add a note"
command = ["sh", "add.sh"]
key = "N"                     # optional: a key crystal and other plugins don't use, or "ctrl+alt+n", or "N t"

[[events]]                    # run by the daemon when something happens
on = "session.waiting"        # or a family of events, like "session.*", or "*" for all
command = ["./on-wait.sh"]

[[panes]]                     # a program the TUI shows
id = "board"
title = "The notes board"
command = ["sh", "board.sh"]
placement = "popup"           # optional: "overlay" (the default), "popup", "split", "zoomed" or "tab"
width = "80%"                 # a popup's: so many cells, or a share of the screen; 80%, left out
height = 30

[[link_handlers]]             # links a Ctrl+click opens with one of its actions
pattern = "^https://github\\.com/[^/]+/[^/]+/issues/[0-9]+$"
action = "add"
```

A command is a list of words, run without a shell from the plugin's directory; a program given as a path is
found from there too. Every command but a build command finds crystal in its environment:

- `CRYSTAL_BIN`: the crystal running it, for crystal's own commands, like `"$CRYSTAL_BIN" send
  "$CRYSTAL_SESSION" "…"`
- `CRYSTAL_SOCKET`: that crystal's daemon
- `CRYSTAL_PLUGIN`, `CRYSTAL_PLUGIN_DIR`: the plugin's name and its directory
- `CRYSTAL_PLUGIN_PROJECT`: for a [project's own](#a-projects-own-plugins), the project's main worktree
- `CRYSTAL_PLUGIN_CONFIG_DIR`: a directory for its settings, like a token, which you fill in:
  `~/.config/crystal/plugin-config/notes/`, made as the plugin is installed
- `CRYSTAL_PLUGIN_STATE_DIR`: a directory for what it keeps as it runs, made before each command:
  `~/.local/state/crystal/plugins/notes/`, the server's own (see [servers](servers.md#servers))
- `CRYSTAL_SESSION`, `CRYSTAL_SESSION_ID`: the session it's about, when there is one
- `CRYSTAL_PROJECT`, `CRYSTAL_WORKTREE`: the project's main worktree, and the worktree, it's about
- `CRYSTAL_LINK`: for an action a [link](#link-handlers) runs, the link

Its settings are every server's, like the config file, and stay when the plugin is removed, for when it's
installed again. What it keeps is each server's, like the sessions it's about, and goes with the server when
`crystal server delete` deletes it.

An action is about the session selected in the TUI; for `crystal plugin run`, the session `--session` names,
or else the one it's run in, or else the current directory. Run from the TUI, what it prints goes to the
plugin's log; `plugin run` prints it, and exits as the action did.

An action's `key` runs it from the sidebar, and from a pane after the prefix: a character, a chord like
`ctrl+alt+n`, or two keys pressed one after the other, written with a space between them, like `N t`, the
footer showing the first while it waits for the second and `Esc` letting it go. It can't be a key crystal's
sidebar has, nor start with one, and two plugins can't share a key or have one's be the first of the other's
two. A key your `[keys]` gives a command, or one of your own, comes before a plugin's.

A pane is a session of its own, started in the plugin's directory. Its `placement` says where it goes:

| `placement` | Where |
|---|---|
| `overlay` | over the panes beside the sidebar, with the keyboard; the default |
| `popup` | in a frame over everything, with the keyboard, `width` by `height`: so many cells, or a share of the screen like `"80%"`, which each is when left out |
| `split` | split off the selected session's pane, to the right, or below with `split = "down"` |
| `zoomed` | split off the same way, and zoomed over its tab |
| `tab` | in a tab of its own, named after its title |

Over the panes or in a popup, it's in `crystal ls` while it's open, and ends when its program does or when you
press `Ctrl+\`. Split off, zoomed or in a tab, it's a session like the others: it stays once its program has
ended, until you kill it. Its `CRYSTAL_SESSION` is its own; `CRYSTAL_PROJECT` and `CRYSTAL_WORKTREE` are the
selected session's.

`crystal plugin pane open <plugin> <pane>` opens one from the command line, or a plugin's own command, about
the session `--session` names, or the one it's run in, beside whose pane a split goes, and prints the session's
name. `--placement` puts it elsewhere, with `--width` and `--height` for a popup and `--right` or `--down` for a
split. Over the panes and in a popup it shows in the TUI used last, and needs one open; split off, zoomed or in
a tab, it goes in the tabs the daemon keeps for the next TUI when none is.

A plugin that names a `min_crystal_version` newer than yours, or `platforms` without yours, won't install;
one already there is listed `unsupported`, saying why, and can't be turned on.

## A project's own plugins

A project can ship plugins in its repository, in `.crystal/plugins/<name>/` of its main worktree, each the same
as any other. A repository mustn't run code on your machine by being cloned or opened, so one is off until you
turn it on for that project, and it hears only that project's events:

```sh
crystal plugin                                   # in the project: its own come after yours
crystal plugin enable lint --project             # shows what it runs, asks, builds it, and turns it on
crystal plugin enable lint --project -C ~/code/app --yes
crystal plugin disable lint --project
```

`--project` means the plugin of that name the project of the current directory ships, or of `-C`'s, rather
than yours; `run`, `pane open`, `build` and `log` take it too. Turning one on writes its name in the project's
`[[project]]` table in the config file, made for it if there's none, and gone again once it says nothing more:

```toml
[[project]]
path = "~/code/app"
plugins = ["lint"]
```

It runs from the copy in the main worktree, whatever branch that's on. Its hooks hear the events whose
`project` is its project, and its startup commands run with everyone's; its actions and panes run from `X`,
where it's listed under the selected session's project, and from the command line. Its actions' keys and its
link handlers aren't used: the sidebar's keys and the links in panes are every project's. `X` turns one off,
but not on: the command line shows what it runs first. Its settings and what it keeps are kept apart from
yours and from another project's of the same name, out of the project's repository.

## Building

`crystal plugin install` runs the plugin's build commands once you've said yes, in turn, from its directory,
as you'd run them, those with `platforms` only on the systems they name. What they print goes to the plugin's
log. A build that fails leaves the plugin installed but off, with the last of what the command printed, and
`crystal plugin` lists it `unbuilt` until `crystal plugin build <name>` works; that turns a plugin that's on off
too, when it fails. crystal runs the commands, not the tools they need: say in your plugin's README which it
needs, like `npm` or `cargo`.

## Startup

Each startup command of each plugin that's on runs once as the daemon starts, after it has brought back the
sessions that were running, and again whenever a daemon starts in place of another, as `crystal
restart-server` does; not when the TUI opens, or a plugin is turned on. It's for restoring what the plugin
keeps and handing it to crystal, then ending: `CRYSTAL_EVENT` is `startup`, and it runs as a hook does, one at
a time with the plugin's hooks, logged, stopped after 30 seconds. One that fails doesn't stop the daemon.

## Link handlers

A `Ctrl`+click on a link in a pane, or copy mode's `o`, goes to the first plugin that's on, by name, with a
link handler whose `pattern` matches the link, the plugin's handlers tried in their order. The handler's
`action` runs in place of your browser, about the session in that pane, with the link in `CRYSTAL_LINK`: open
an issue in a pane of the plugin's own, say, or have an agent look at it. The pattern is a regular expression,
matched anywhere in the link unless `^` and `$` pin it. A file's path in the text isn't one of their links: it
opens in your editor. `X` lists each plugin's handlers under it, and `crystal plugin run <name> --link <url>`
runs the action its handlers give a link, to try them.

## Events

| Event | When |
|---|---|
| `session.started` | a session starts, or starts again: picked up where it was, its `from` says what picked it up, `conversation` and its id or the command that resumes it |
| `session.renamed` | a session gets another name |
| `session.working` | a session's agent starts working on a turn |
| `session.waiting` | a session's agent comes to wait on you |
| `session.done` | a session's agent finishes a turn nobody was watching |
| `session.idle` | a session's agent is at its prompt, its turn seen |
| `session.ended` | a session's program ends, or the session is killed |
| `session.start_failed` | a session can't start again after a [restart](sessions.md#after-a-restart): its directory or its command has gone; its `status` says why |
| `session.removed` | a session leaves the list: killed, or its worktree removed |
| `session.archived` | a session is stopped and kept in the archive: `A`, or `crystal archive` |
| `session.unarchived` | a session is started again from the archive: `Z`, or `crystal unarchive` |
| `session.opened_in_terminal` | a background task is opened in a terminal, picking its conversation up: `C`, or `crystal tasks terminal` |
| `session.claimed` | an agent takes over saying what a session is doing, with [`crystal report`](agents.md#teaching-crystal-about-your-agent) |
| `session.released` | it lets go: `crystal report --release`, or it left and the shell is back in front |
| `subagent.started` | a session's agent starts a subagent, as its hooks say: its `subagent`, with its `id` and `agent_type` |
| `subagent.stopped` | that subagent finishes |
| `session.message` | a session is sent a message: by another session with `crystal send`, which its `message` names, or by you |
| `session.bell` | a session's program rings the terminal's bell while nobody's watching it |
| `session.copy_dropped` | a session's program copies while nobody's watching it, which crystal doesn't put on your [clipboard](tui.md#zoom-copy-mode-and-search) |
| `task.opened` | a task is made: given to a session as it starts, made to start later, or opened again by a follow-up |
| `task.started` | a task made to start later starts, in a session of its own |
| `task.waiting` | a task's agent ends a turn with the task still open: it waits on you |
| `task.reminded` | an agent ending its turn with its task open is reminded to close it, and carries on |
| `task.closed` | a task closes, done, failed or cancelled |
| `task.artifact` | a file is kept with a task as it closes: one `crystal done --artifact` named, or its worktree's handoff file |
| `run.started` | a background task starts a run of Claude: its prompt, or a follow-up |
| `run.tool_use` | a background task's Claude uses a tool: its `run`'s `tool`, with its `name` and `gist`, one for each call |
| `run.asking` | a background task's Claude asks you for a permission |
| `run.answered` | you answer it: allowed, allowed always, or denied |
| `run.interrupted` | you stop a background task's run halfway |
| `run.ended` | that run ends, with what the task has cost |
| `flow.started` | a flow run starts |
| `flow.step_started` | a flow run starts a step |
| `flow.step_ended` | a step's run ends: done, at its gate, or failed |
| `flow.gate` | a flow run waits at a gate for you |
| `flow.gate_answered` | you approve a gate, or send the run back |
| `flow.ended` | a flow run ends: every step done, one failed, or you cancelled it |
| `worktree.created` | crystal makes a worktree |
| `worktree.removed` | crystal removes one |
| `worktree.hook_failed` | a [worktree hook](worktrees.md#setting-one-up) failed, or ran too long |
| `handoff.added` | a note goes in a worktree's handoff file: `crystal handoff`, or a task closing there |
| `memory.added` | an entry is added to a project's memory: remembered, or by the distiller |
| `memory.forgotten` | an entry is forgotten: by you, or by the distiller, once one gone stale no longer holds |
| `memory.stale` | all an entry names is gone from the code, or naming nothing to look for, every file it's about, as the daemon finds hourly and as each task closes |
| `memory.promoted` | an entry is written into the project's CLAUDE.md or AGENTS.md, which its `file` names |
| `memory.changed` | an entry's kind is changed, by `crystal memory kind`, `c` in the memory view or the distiller |
| `memory.distilled` | the [distiller](memory.md#the-distiller) has read what a session did: its `distill` says how many entries it `added`, found `again`, `rechecked` of those gone stale, `superseded` (updated or retired) of those it was shown and `rejected`, and what it cost |
| `memory.distill_failed` | the distiller couldn't: its `distill`'s `failed` says why |
| `memory.merged` | `crystal memory dedupe --apply` merged entries that say the same thing into one: `memory` is the one kept, as it is now, and `from` the ids of those that went into it |
| `memory.superseded` | an entry stopped holding, another said in its place, by `remember --replaces`, `memory retire`, `memory reconcile --apply` or the distiller: `superseded` is it as it was, with `by` the entry that holds in its place (its own id when it was updated) and `why`, and `memory` that entry as it is now |
| `memory.restored` | `crystal memory restore` put an entry that stopped holding back as it was: `memory` is it |
| `backlog.added` | an item goes on a project's backlog |
| `backlog.closed` | an item is marked done |
| `plugin.paused` | a plugin is paused for failing |
| `daemon.handed_over` | the daemon is handed over to another crystal, its sessions carrying on (see `restart-server`) |
| `daemon.restarted` | the daemon, restarted cold, has started the sessions that were running again: its `daemon` says how many came back (`sessions`) and which couldn't (`failed`) |
| `tab.created` | a TUI makes a tab: `t`, `crystal tab new`, a layout put back |
| `tab.closed` | a TUI closes one; its `tab` is as it was |
| `tab.renamed` | a tab is named, or its name taken back: its `from` is the name it had, empty for none |
| `tab.moved` | a tab moves to another place among the tabs: its `from` is the number it had |
| `tab.focused` | another tab comes to the front: its `from` is the number of the one in front before |
| `pane.focused` | the selection settles on another session, the one you're on, in the sidebar or a pane: its `session`, the `tab` it's in, and the session you were on as `from` |
| `pane.moved` | a session moves to another tab: its `from` is the number of the tab it left |
| `layout.updated` | a tab's panes change: split, closed, resized, swapped, zoomed, floated or evened out; its `tab` has them |
| `project.added` | a project goes on crystal's list: `crystal project add`, or a session starting in it |
| `project.removed` | a project is taken off it |
| `project.focused` | the selection settles on a session in another project: its `from` is the project you were in |

A hook gets the event as a line of JSON on its standard input, the same as the [event log](events.md) keeps it,
and its name in `CRYSTAL_EVENT`:

```json
{"seq":412,"at":1790949076244,"event":"session.waiting","project":"/code/app","session":{"name":"claude-2",
 "id":"k3x9…","command":["claude"],"cwd":"/code/app","project":"/code/app","worktree":"/code/app",
 "branch":"main","activity":"waiting","task":"Fix the login redirect","status":"waiting"},"from":"working"}
```

`task.closed` has a `task`, with its `goal`, `session`, `project`, `branch` and `outcome` (whether it `failed`,
its `summary`, and when it `closed`). The tab and pane events have a `tab`, the tab as `crystal layout --json`
has it: its `number`, `name`, the `sessions` in it, the one `selected`, and its `panes`. A TUI tells what
changed in its tabs and panes once they've held still for a moment, so `j` held down through the sidebar, or a
border dragged across the screen, is one event, about where it ended; with no TUI open, the daemon tells what
a [layout command](tui.md#tabs) changed, but for the session you're on, as you aren't on one. The worktree events have a `worktree`, with its `path`, `branch` and
`project`, and for `worktree.hook_failed`, `why`. `session.message` has a `message`, with its first `line` and,
when another session sent it, that session's name (`from`) and id (`from_id`). The rest are under
[events](events.md#the-event-log).

The same JSON is in `CRYSTAL_EVENT_JSON`, and what happened in a line in `CRYSTAL_EVENT_TEXT`, what it's about
and what the timeline says of it, for a script that wants no `jq`: `claude-2: working → waiting`, `reviewer:
done: two risks in the retry loop`, or the event's name where there's nothing more to say. `crystal plugin
events` lists every event with when it happens.

`crystal plugin run <name> --event <event>` runs the plugin's hooks on that event, here and now, whether the
plugin is on or not, on a made-up event with everything its kind carries, about the session `--session` names,
or the one it's run in, or a made-up one. `--json` gives the event, or what of it to put over the made-up one,
as a JSON object, or `-` to read it from standard input, so a line of `crystal events --json` runs them on
something that happened; its `event` says which, unless `--event` does. What they print is printed, and it
exits as the first that failed.

A plugin's hooks run one at a time, in the order things happened, and what they print goes to its log, kept in
crystal's state directory. A hook still running after its `timeout_secs`, or its plugin's, 30 seconds unless
either says, is stopped. After 5 failures in a row, startup commands included, the plugin is paused, with a
notification, and `crystal plugin` shows it `paused` until `crystal plugin enable <name>` turns it back on.

## Examples

[`examples/plugins/`](../examples/plugins/) has plugins to copy, or install as they are with `crystal plugin
install examples/plugins/<name>`. crystal's tests install each and run its hooks.

| Example | Hears | Does |
|---|---|---|
| [`event-log`](../examples/plugins/event-log/) | `*` | keeps every event as a line of JSON; its `follow` pane, split off below, shows them as they come |
| [`slack`](../examples/plugins/slack/) | `task.closed`, `flow.gate`, `flow.ended`, `plugin.paused` | posts `$CRYSTAL_EVENT_TEXT` to a Slack incoming webhook, its URL in its settings directory, unless a task or a flow run ended done |
| [`worktree-env`](../examples/plugins/worktree-env/) | `worktree.created` | copies the project's `.env` files into each worktree crystal makes: one to ship in a repository's `.crystal/plugins/` |

## Security

A plugin is code that runs as you, with everything you can reach: your files, your keys, your logins. Its
build runs as it's installed, and its hooks and startup commands in the background, whenever something happens
or the daemon starts. Add only plugins you'd run as a script of your own. `crystal plugin install` shows every
command a plugin would run, its build and startup commands and the actions its link handlers run included, and
asks before it installs it, and installs it switched off. A project's own plugin never runs until `crystal
plugin enable --project` has shown you the same and you've said yes; after that, it runs whatever the
repository's main worktree has, so turn it on only for a repository whose changes you'd run.
