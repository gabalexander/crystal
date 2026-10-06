# Agents driving agents

<sub>[← README](../README.md#documentation)</sub>

Every session knows how to reach its daemon, so an agent can run crystal commands too: start a second agent,
hand it work, wait for it, and read what it said. Here a Claude Code session gets a review of its change:

```sh
crystal new -d -n reviewer claude                         # a second Claude, in the background
crystal send reviewer "Review the diff on this branch" --wait   # prints done, or waiting if it asks something
crystal read reviewer --lines 40                          # the end of its answer, or its question
crystal send-keys reviewer 1 --wait                       # answer a question: the first choice
```

- [Sending](#sending)
- [Waiting](#waiting)
- [Keys, screens and processes](#keys-screens-and-processes)
- [Streams, snapshots and the schema](#streams-snapshots-and-the-schema)
- [Sessions as JSON](#sessions-as-json)
- [Laying out the TUI](#laying-out-the-tui)
- [A skill for Claude Code](#a-skill-for-claude-code)

## Sending

`send` types the way a person does: the text first, marked as a paste when the program asks for that, then
Enter on its own, so an agent takes it as a prompt and not as pasted text. `-` as the text reads it from
standard input: `git diff | crystal send reviewer -`. An agent at its prompt is watched as it's typed into:
Enter goes once its screen shows the text, and is pressed again, three times in all, while the agent neither
starts on it nor changes its screen, as when it was still reading the paste. One that never takes it has
stalled: `send` fails with an error that starts `agent_prompt_stalled:`, with exit status 3, rather than
returning as if the text went, and the text may still be in the agent's input, so `read` it before sending it
again. `wait` returns once the agent isn't working: `done`,
`waiting` when it asks something, `idle`, or how its program exited. It takes a `--timeout` in seconds, and
gives up when that runs out, with exit status 2; anything else that goes wrong, a mistyped flag included, is
1, so 2 always means "not yet". `--quiet` (`-q`) prints nothing once it's there. `send --wait`, `send-keys
--wait`, `task --wait` and `flow wait` give up the same way. A program that doesn't say what it's doing counts
as busy until it ends.

`send --wait` waits for the turn the text starts, never one that ended before it: it listens to the session
from before the text goes, and looks at its screen once the text shows there. An agent that wasn't working has
five seconds from then to be seen starting on it, working, asking something, or its program ending, or to change
its screen. One whose screen changes took the text, though its turn wasn't seen: one too short to fall between
two of crystal's looks, or one getting under way on a loaded machine. Once the five seconds are over and its
screen has held still for two, the wait ends on how it stands. One that did neither, its screen as it was once
the text went in, has stalled, `agent_prompt_stalled:` with exit status 3 as above, rather than taking what the
agent said about its turn before for an answer. A stall doesn't prove the agent never got the text, which a turn
shorter than the moment the screen is given to show it can take, so `read` it before sending the text again.
`--timeout` counts from before the text goes, the typing and the Enters included. An agent working already takes the text once its turn is over, and
that turn's end may be what ends the wait. A program that doesn't say what it's doing is waited on until it's
seen starting or it ends, or its screen changes and holds still. `send-keys --wait` waits for the turn the keys
start the same way, but never stalls: keys that change nothing on the screen in five seconds end the wait on how
the agent stands, as they can carry on a turn its agent doesn't say it's working on, like an answer to a
permission.

| Exit status | `wait`, `send --wait`, `send-keys --wait`, `task --wait`, `flow wait` |
|---|---|
| 0 | it got there, and printed where |
| 1 | anything else went wrong: no such session, it ended or was killed first, a mistyped flag |
| 2 | `--timeout` ran out first: not yet |
| 3 | `send`, with `--wait` or not: the agent never took what it was sent, neither starting on it nor changing its screen (`agent_prompt_stalled:`) |

An agent asking you something takes nothing `send` types, since the text would land in its question: `send`
refuses with an error that starts `agent_blocked:`, saying what it asks and how to answer it, with `crystal
answer` for a background task or in its pane or with `send-keys` for one in a terminal. `--force` types it
anyway. An agent waiting only because it ended a turn with its [task](tasks.md#tasks) open is at its prompt, and takes
it. `send-keys` is never refused, since that's how a question is answered.

Sent from another session, a message says so on a line ahead of it, `[crystal] Message from session "scout",
working on task "Port the codec":`, so the agent knows who asks, and that `crystal send` answers. Such a
message loses its control characters but for line breaks, and is cut at 8 KiB. A session may send 20
messages a minute, the most crystal allows: the next is refused, since two agents answering each other are
most likely in a loop. So is a message that only acknowledges: under 20 characters, made of nothing but `ok`,
`thanks`, `received`, `ack`, `done` and `noted` (any case, any punctuation), or with no letters at all, like
`👍`. `ok, 3 tests fail` goes; `ok, thanks!` doesn't. A session can't send to itself. From you, a script or the
TUI's `Space`, the text goes as it is, with no line ahead of it and no limit. Each message is a
`session.message` [event](events.md).

`send --interrupt` stops the run a [background task](tasks.md#background-tasks) is in the middle of first, waits for it
to end, and sends the text as its next prompt: a change of course without waiting for the turn. An agent in a
terminal is stopped by its own key, which crystal doesn't press for another session: `crystal send-keys
<session> Escape`.

## Waiting

`wait` can wait for something else instead:

```sh
crystal wait reviewer --until waiting         # until it asks something; prints waiting
crystal wait reviewer --until done,idle       # any of them: working, waiting, done, idle, ended, closed
crystal wait server --output 'listening on'   # until a line on its screen matches; prints the line
crystal wait t12                              # until task t12 closes; prints done, failed or cancelled
crystal wait t12 --until waiting,closed       # until its agent asks something, or the task closes
```

`--until` returns at once if the session is there already, and catches a state it's in only a moment, like
`working`. A turn that ends while someone watches it is `idle` at once, never `done`, so a script that doesn't
mind waits for `done,idle`. `ended` (or `exited`) waits for its program to end; ending first, when that isn't
what it waits for, is an error. `--output` takes a regular expression, matched a line at a time against the
screen and the 200 rows above it, so output already there counts. Both take `--timeout`. Waits listen to the
daemon's [events](events.md) rather than asking it again and again, and `--output` looks each time the program
writes.

`wait` takes a [task](tasks.md#tasks)'s number, like `t12`, where it takes a session's name, and waits until the task
closes, printing how it went: `done`, `failed` or `cancelled`. It follows the task whichever session works on
it, from a task made to start later to one whose session has gone, and returns at once for one that's closed
already. `--until closed` waits for a session's task to close the same way; a session with no task is refused.
Beside other states, as in `--until waiting,closed`, those are about the session working on the task. A
session that ends, or is killed, with its task open closes it, failed or cancelled, which the wait prints
rather than failing. A closed task isn't always over: a follow-up opens a background task again.

## Keys, screens and processes

`send-keys` presses keys instead, the way tmux's does: key names like `Enter`, `Escape`, `Tab`, `Up`, `Down`,
`BSpace`, `C-c` or `M-x`, and any other word typed as keys. That's what answers an agent's question, since
agents don't act on a pasted answer. With `--wait`, it waits for the turn the answer lets carry on. An agent
crystal [stopped idle](sessions.md#archiving-and-idle-agents) is started again first, as `send` starts it.

`read` prints the screen as text, each row without the blanks at its end:

```sh
crystal read reviewer --lines 40              # the last 40 rows that aren't blank
crystal read reviewer --history               # and what scrolled up off it before
crystal read builder --since 10m              # only what it wrote in the last 10 minutes, history and all
crystal read builder --unwrap                 # a long line as one, not the rows it wrapped onto
crystal read reviewer --ansi | less -R        # its colors, bold, italic and underlines kept
```

`--since` takes a while back (`30s`, `10m`, `2h`) or a time (`14:00`, `2026-10-01T09:30`), as `crystal events`
does. The daemon keeps the last MiB of what each session's program wrote, with when it came, and lays what came
since then out on a screen of its own, the session's size: what the program wrote, not the screen it drew
before, so a line or two from up to a second before may come in. When that MiB doesn't reach back so far, or
the program wrote before a `restart-server` handed it over, `--since` prints the history and the screen whole.
`--ansi` keeps the style of the text as SGR codes, and nothing that moves the cursor or makes a link.

`crystal clear` clears a session's screen and history but for the line its cursor is on, sending its program
nothing: the one it's run in, or the one `-n` names. It's the TUI's
[`clear-pane`](tui.md#zoom-copy-mode-and-search), and like it, leaves a program on the alternate screen alone.

`crystal process-info <session>` (or `ps`) lists what runs in its terminal: the processes in front, the job
its keys go to, a line each, with its pid, its name, the directory it works in and its command; `--json` adds
the session's own program's pid and the foreground process group.

## Streams, snapshots and the schema

A program can watch a session's terminal, or drive it, as a stream of JSON lines:

```sh
crystal observe reviewer                      # its output as it comes, read-only
crystal control builder --rows 40 --cols 120  # and commands on standard input
```

Each prints `{"type":"start","session":…,"id":…,"rows":…,"cols":…,"running":…}`, then each `{"type":"output",
"data":…}` the program writes, base64, the first drawing the screen as it is on a fresh terminal of that size,
and at the end `{"type":"closed","reason":…}`: `ended` when its program has, `released` when `control` lets
go. A `restart-server` cuts the stream, which attaches again and says so with a new `start`, for a reader to
begin its screen afresh. Neither counts as you watching the session, so its notifications and its `done` stay as
they were, and neither resizes it, unless `control` is given `--rows` and `--cols`. `control` reads a command a
line: `{"type":"input","text":"ls\r"}` or `{"type":"input","data":"<base64>"}` writes bytes to the program as
they are, `{"type":"keys","keys":["Enter","C-c"]}` presses keys by name as `send-keys` does,
`{"type":"resize","rows":40,"cols":120}` resizes it, and `{"type":"release"}`, or the end of its input, lets go.
A command it can't carry out is a `{"type":"error","message":…}` line, and it goes on.

`crystal api snapshot` prints everything a client that keeps its own picture of crystal starts from, as one
JSON object: crystal's `version`, the daemon's `socket`, `seq`, the latest event's, `sessions` as `ls --json`
lists them, `layout` as `crystal layout --json` prints it, `projects`, the `tasks` not closed, every flow run
in `flows`, and the `archived` sessions. `crystal events --follow --after <seq>` then carries on from it with
nothing missed: an event says what changed, and the client asks again for what it shows. It never starts the
daemon.

`crystal api schema` says what the schema of crystal's socket protocol covers, the one bundled with that crystal:
`--json` prints the whole JSON Schema (draft 2020-12), and `--output PATH` writes it to a file. It's
[`docs/crystal-api.schema.json`](crystal-api.schema.json) too. Its `schemas` name each message: the
`request` a client sends, a JSON object a line with its kind in `type` and the `version` of the crystal sending
it, which must be the daemon's; the `response` the daemon answers with; each `event` a `subscribe` streams,
`crystal events --json` prints and plugins' hooks are given, its kind in `event` with when it happens; the
`layout_order` and `layout_report` lines a TUI taking layout orders and the daemon trade; and the `snapshot`
`crystal api snapshot` prints. `$defs` holds every type they're made of, described as crystal reads it: a field
it can do without is optional, though crystal may always write it.

## Sessions as JSON

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
    "front": { "kind": "agent", "program": "claude", "name": "Claude Code" },
    "status": "waiting"
  }
]
```

`state` is `"running"`, `{"exited": {"code": 3}}` or `{"signaled": {"signal": "Terminated"}}`; `activity` is
`null` for a program that doesn't report what it's doing; `worktree` is `null` outside a git repository, its
`branch` `null` on a detached HEAD, but for a rebase under way the branch being rebased, and it carries
`in_progress` only while git is in the middle of something there, `merge`, `rebase`, `cherry-pick` or `revert`;
`front` is what's in front in the terminal: `{"kind": "agent", …}`, `{"kind": "shell", "name": "zsh"}`,
`{"kind": "program", "name": "vite"}` or `{"kind": "task"}`, and `null` until it's been looked at.
`task` is `null` for a session started with nothing to do, and otherwise holds its [task](tasks.md#tasks): `id`,
`goal`, `waiting` while it waits on you, once it's closed, `outcome` with `failed`, `cancelled` and
`summary`, and when it has them, its `accept`ance criteria, its `pull_request` and its `issue`. `asking` holds
the permission a [background task](tasks.md#background-tasks) waits on you for, `tool` and `gist`, and is `null`
otherwise; `context`, how full a background task's conversation is, `tokens` of its model's `window`, once
Claude has said. `reporter` holds an agent that [says what it's doing
itself](agents.md#teaching-crystal-about-your-agent): its `agent` name, its last `message` and its `resume` command;
while it's there, `front` is that agent. `output_waits`, there only while there are some, counts the `crystal
wait --output` looking at its screen, so a script can tell its wait has reached the daemon. New fields may
appear; none goes away. With no daemon running, it prints `[]`.

## Laying out the TUI

The command line lays out the tabs and panes too, so an agent can put the session it started on screen
beside its own, or a script can set up a tab for a review, with the TUI open or not:

```sh
crystal new -d -n tests cargo test
crystal pane split tests                  # beside the pane of the session this runs in; --down below it
crystal pane split logs --beside server --down --ratio 0.7   # server keeps 70% of the room
crystal pane split -e PORT=4000           # no session named: a new shell, split off; prints its name
crystal pane focus tests                  # select it, its tab in front, and type into it; or left, right, up, down
crystal pane focus tests --raise          # the same, and bring the TUI's terminal to the front, as a notification's click does
crystal pane resize left 8 -n tests       # move a border of its pane, as R does; 4 columns or 2 rows by default
crystal pane swap right -n tests          # swap its pane with the one to its right, as L does; or with another session's
crystal pane ratio 0.3 tests              # give its side of the split it's in 30% of the room; --right or --down for that way's
crystal pane close tests                  # close its split, or put its float back
crystal pane zoom reviewer                # zoom its tab on it; --off puts the panes back
crystal pane float logs                   # float it over its tab's panes; --off puts it back
crystal pane equalize                     # even out the panes, as = does in resize mode
crystal tab new review                    # a tab after the others, in front: sessions started now go in it; prints 2
crystal tab select 1                      # a tab by its number or its name
crystal tab rename 2 checks               # an empty name takes it back to its number
crystal tab move reviewer review          # move a session to another tab, as > does
crystal tab reorder review 1              # move a tab to be the first, as { and } do a place at a time
crystal tab close review --kill           # a tab with sessions closes only with --kill, which kills them
crystal layout                            # each tab's sessions and how its panes split the room; --json
crystal layout export > dev.json          # the tabs as a layout file, with what starts each session; --tab for one
crystal layout apply dev.json             # lay them out as the file says, starting what isn't there; --replace
crystal title set "deploying"             # the title of the TUI's terminal, in place of the settings' one
crystal title clear                       # back to the settings' one
crystal sidebar move tests --up           # a place up among the sessions in its worktree, as Alt+k does; --down
crystal sidebar move tests --before lint  # just before another of them; --after
crystal sidebar move --project --down     # the project this directory is in, among the projects, as Alt+J does
crystal sidebar move --project ~/web --before ~/app   # a project just before another, each by a directory in it
```

A command about a session works on the tab that holds it, whether it's in front or not, and leaves the tab in
front where it is: an agent in another tab lays out its own without taking you there. Only going somewhere
moves you: `tab new`, `tab select` and `pane focus`. Without a session named, a command is about the session
it runs in, or, run outside crystal, the selected one. `pane split` splits the pane that session has of its
own, or else the selection's pane, which it selects if that pane shows something else; the session split off
moves into that tab, out of any pane it had. With no session named, `pane split` starts a new shell to split
off, in the directory it's run in or `--cwd`, with any `--env` variables, as `crystal new` does, and prints its
name. A command that can't be carried out says why, the way the footer would: no room for another pane, a
session that isn't on screen. `title` needs a TUI open: with none, there's no terminal to give the title.
`sidebar move` puts a session in [the order](tui.md#the-order) of your own, among the sessions beside it in its
worktree, or with `--project`, a project among the projects.
`pane swap` trades the places of two panes in a tab, the splits and how big each is staying as they are:
the session's and the one that way from it, or another session's. `pane ratio` gives the side a session's
pane is on, of the split nearest above it, a share of that split's room, whichever side it is: or of the
nearest split side by side with `--right`, or one above the other with `--down`.

The command goes through the daemon to the TUI you used last, the one where you last pressed a key, clicked
or brought its terminal to the front, and waits for it to answer, a few seconds at most. When `restart-server`
hands the daemon over, each TUI offers itself to the new one at once, saying when you last used it, so
commands carry on going to the same one. What it changes is kept like any change you make, so it's there when
the TUI opens again. With no TUI open, the daemon carries the command out itself, the way the TUI would, on
the tabs as the TUI last kept them, starting if it isn't running; a tab closed with `--kill` has its sessions
killed. The next TUI to open shows the tabs as the commands left them.
`crystal layout --json` prints the tabs in their order, each with its `number`, `name`, whether it's
`current` and `zoomed`, its `sessions`, the one `selected`, the one `floating`, and its `panes`: either
`{"kind": "pane", "session": "tests"}`, with `"selection": true` for the pane that follows the selection, or
`{"kind": "split", "way": "right", "ratio": 0.5, "first": …, "second": …}`, `way` being `right` for side by
side and `down` for one above the other, and `ratio` the first side's share. Beside the tabs, `presence` says
whether you're at crystal, as the TUIs' terminals say of their focus: `here` while one has it, `away` once
every one has lost it, and `unknown` with no TUI open or one whose terminal doesn't say.

`crystal layout export` writes the tabs as a layout file, a JSON document to keep with a project or take to
another machine, and `crystal layout apply` lays the tabs out the way one says, from a file or standard input,
starting the sessions it names that aren't there, as herdr's `layout.export` and `layout.apply` do:

```json
{"tabs": [{
  "name": "dev",
  "current": true,
  "panes": {"kind": "split", "way": "right", "ratio": 0.6,
    "first": {"kind": "pane", "session": "editor", "selection": true, "cwd": "~/repo", "command": ["nvim"]},
    "second": {"kind": "pane", "session": "tests", "cwd": "~/repo", "command": ["sh", "-c", "cargo test"],
      "env": {"RUST_LOG": "debug"}}},
  "sessions": [{"session": "notes", "cwd": "~/notes"}],
  "floating": "logs"
}]}
```

A file is in the shape `crystal layout --json` prints, which applies as it is, with more said about a session
where it's named: a pane's `cwd`, `command` and `env`, and in a tab's `sessions` and its `floating`, a
session's name or an object with the same, its name as `session`. All but the tabs can be left out, and
without a pane marked `selection`, the first follows the selection. A session that's there, running or
ended, is laid out as it is; one that isn't starts under its name when the file says how: its `command`, or
a shell, in its `cwd`, or the directory `apply` runs in (`~` and relative paths work), with its `env` over
the environment `apply` runs with. A pane naming no session starts a new one the same way, named as `crystal
new` names one, but for the pane that follows the selection, which with nothing said shows whatever's
selected. What can't start, or the file doesn't say how to, is left out, saying so; `apply` prints the name
of each session it started.

Each tab of the file lays out the tab with its name, or a new one after the others when it has no name or
there's none, so a tab with a name is laid out in place when the file is applied again. The sessions it
names move there, out of any pane they had; those in the tab already that it doesn't name stay, off the
screen. The tab marked `current` comes to the front; with none, the tab in front stays. `--replace` puts the
file's tabs in place of every tab instead, as restoring a [saved layout](tui.md#layouts) does, the sessions it
doesn't name joining the tab in front: `crystal layout export > tabs.json`, then later `crystal layout apply
--replace tabs.json`, puts the tabs back the way they were and starts what has gone since. An export writes
each session's command as it was started, an agent's without its first prompt, and its directory, but not
its variables, which it doesn't know; a background task, with no terminal, by its name alone. A file that
can't be applied, with two tabs of one name, say, a session in two places or a ratio that isn't a share,
changes nothing and says why.

## A skill for Claude Code

Claude Code learns all of this from a skill: when to hand work to another agent, the commands, what each
status means, and the traps, like answering a question with `send-keys` rather than `send`.

```sh
crystal skill --install   # into ~/.claude/skills/crystal, or $CLAUDE_CONFIG_DIR/skills/crystal
crystal skill             # or just print it
```

The install script installs it when it finds Claude Code (its `claude` command, or `~/.claude`), and so does
`make install`; `CRYSTAL_NO_SKILL=1` leaves it out of either.

Every Claude Code session crystal starts is also told, on top of its system prompt, to work on several things
at once as sessions of crystal's, one `crystal new -d -w <branch>` each, rather than in worktrees or subagents
of its own: a session shows in the sidebar with its status, its diff and its screen, where you can step in,
while a worktree Claude Code makes for itself shows only once it's left behind. It's told too to show you a
file you ask to see with [`crystal open`](code.md#files-an-agent-shows-you), rather than paste it into its answer, and
never to open one unasked.

`--install` won't write over a skill file you've changed; `--force` does. The skill lives in
[`skill/SKILL.md`](../skill/SKILL.md), and each crystal carries its own copy. After an upgrade, the daemon brings
the skill up to date as it starts, when the copy installed is one an earlier crystal wrote and nobody has
changed since; it never installs the skill where it isn't, or writes over one you've changed.
