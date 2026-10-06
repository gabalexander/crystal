# Events and the timeline

<sub>[← README](../README.md#documentation)</sub>

Everything that happens in crystal is an event, written down in a log you can read, follow and listen to, and
which the TUI's timeline shows.

- [The event log](#the-event-log)
- [Listening on the socket](#listening-on-the-socket)
- [How long it's kept](#how-long-its-kept)
- [Timeline](#timeline)

## The event log

The daemon writes down everything that happens in an event log, kept in crystal's database
(`~/.local/state/crystal/crystal.db`): sessions starting, working, waiting and ending, tasks opening and
closing, background runs, what they asked and what they cost, flows, worktrees, memory and the backlog, and
the TUI's tabs and panes and the session you're on.
`crystal events` prints it, one line each, the oldest first:

```sh
crystal events                        # all of it
crystal events --since 2h             # or 30m, 3d, 14:00, 2026-10-01T09:30
crystal events -n reviewer -k 'task.*'   # one session, through its renames; kinds or families, repeatable
crystal events -C ~/code/app --json   # one project's, as JSON lines
crystal events --follow               # new ones as they happen; with --since, catch up first
crystal events --task t12             # one task's: the task, and its session while it works on it
crystal events --limit 20             # only the newest 20; with --follow, of those before the new ones
crystal events --after 4120 --follow  # those after that seq, like the one `api snapshot` gives
```

```
14:03:07     session.started     reviewer  claude 'Review the diff on this branch'
14:03:07     task.opened         reviewer  Review the diff on this branch
14:03:09     session.working     reviewer  idle → working
14:05:40     session.waiting     reviewer  working → waiting
14:05:52     session.working     reviewer  waiting → working
14:06:02     session.done        reviewer  working → done
14:06:15     task.closed         reviewer  done: two risks in the retry loop
```

The time is `14:03:07` today, `09-24 14:03` earlier in the year, and the date before that. In the TUI, `a`
shows the same lines as the [timeline](#timeline).

Each line of `--json` is one event, the same JSON the log keeps and plugins get: its `seq` (1, 2, 3…, never
going back), `at` (milliseconds since the Unix epoch), its name as `event`, the `project` it's about, the
`session` (its `name`, `id`, `command`, `cwd`, `project`, `worktree`, `branch`, `activity`, `task`, and its
number as `task_id`, `status`, as `ls` words it, and `reporter` while an agent that reports for itself holds it), and what its kind carries:
`from` (a renamed session's old name, what its agent was doing before, or the agent that let go; what picked a
session started again up where it was, `conversation` and its id or the command that resumes it; a renamed
tab's old name, a moved tab's number before, the number of the tab a session moved from, or the session or
project you were on before), `tab` (the tab, as [`crystal layout --json`](tui.md#tabs) has it), `task` (with
its `id`, `pending`, `waiting` and, once closed, its `outcome` and the `artifacts` kept with it), `run`
(`prompt`; `asking`, with its `tool` and `gist`, and the `decision`; then `failed`, `answer` and `cost_usd`),
`flow` (`run`, `flow`, `goal`, `step`, `state`, `said`, `cost_usd`), `worktree`, `handoff` (the file's `path`
and the `note`'s first line), `artifact` (a kept file's `kind`, `name`, `path` and `bytes`), `memory` (the
entry), `file` (the file an entry was promoted into), `distill` (`added`, `again`, `rejected`, `cost_usd`, or
why it `failed`), `backlog` (the item) or `plugin` (`name` and `why`); a `run`'s `tool` has its `name` and
`gist`. New fields and events may appear; none goes away.
The events are listed under [plugins](plugins.md#events).

## Listening on the socket

A program can listen on the daemon's socket, as `--follow` does, with one line of JSON:

```json
{"type": "subscribe", "version": "0.3.0", "filter": {"kinds": ["session.waiting", "task.*"], "session": "reviewer"}, "since": {"seq": 41}}
```

`filter` takes `kinds` (names or patterns), a `session` by name or id, a `project` by the path of its main
worktree and a `task` by its number, all optional. `since` is `{"seq": N}` for the events after that one, or `{"at": ms}` for those from
that time; leave it out for new ones only. The daemon answers `{"type":"subscribed","seq":N}`, sends what the
log has from `since` up to that `seq`, then each new event as it happens, one line each, so a client that
reconnects with the last `seq` it saw misses nothing. One that falls more than 4096 events behind gets a last
`{"type":"error",…}` line and is let go. `version` is crystal's own: the daemon refuses another's.

## How long it's kept

The log keeps 30 days, or `keep_days` under `[events]` in the config (`0` keeps everything), and never more
than 50,000 events; the daemon prunes it as it starts and every hour after, and the numbers never go back,
however much is pruned. Events from outside the daemon, like `crystal remember`, reach the log through it,
so one done with no daemon running isn't written down.

## Timeline

`a` in the TUI opens the timeline: the event log read back, the newest first, a line an event, the way
`crystal events` prints it: when, what happened, in the color of how it went, the session or flow run it's
about, and what it says. It's live: while it's open, new events come in on top, and the bar stays on the line
it was on. Type to filter the lines by anything in them (`fixer`, `task.closed`, `failed`, a branch), and
`Tab` and `Shift+Tab` narrow them to one kind: sessions and worktrees, tasks with their runs, handoff notes
and the backlog, flows, memory, the tabs, panes and focus of the layout, or the others. `↑` and `↓` move; the line the bar is on is read whole under
the list, with everything its event carries, and `PgUp` and `PgDn` scroll it. `Enter` goes to the session the
line is about, whatever it's called now (for a flow run, its latest step's); `Esc` clears the filter, then
closes. The timeline reads the log a page at a time, and further back as the bar reaches the end.

A timeline can be of one thing, too. `I` opens the selected session's: everything about it, whatever it was
called then, and the messages it sent. `Ctrl+S` goes on to its task's (what the task did and kept, and its
session's events while it had the task), then its project's (every session, worktree, flow, memory entry and
backlog item there), then everything, and round again; the heading says which it shows, and `a` starts at
everything and goes the same way. A session's right-click menu has **its timeline** and **its task's
timeline**, and a project's heading **its timeline**; `session-timeline`, `task-timeline` and
`project-timeline` are in the [command list](keys.md#the-command-list) to give keys to. On a worktree with no
sessions, `I` opens its project's.

`U` lists everything that needs you now, in every tab, the most urgent first: background tasks asking for a
permission, flow runs at a gate, tasks whose agent ended its turn with the task still open, agents asking you
something, sessions that couldn't start again after a [restart](sessions.md#after-a-restart), with why, then agents that finished
a turn you haven't seen. Within each, whatever has waited longest comes first, and each thing has one row.
`y`, `n` and `Y` answer a permission where it stands, `g` and `f` a gate (go on, or send it back with your
notes), as on the session's own row, and `r` starts again a session that couldn't start, once you've put
right what stopped it; the list stays open, and an answered row leaves it. `Enter` goes to the session, and `Esc` closes the list.

When you come back to crystal, the footer says what happened while you were away, in one line:

```
while you were away: 2 tasks done · 1 failed · 3 sessions finished · 1 needs you
```

Only what isn't nothing is said, a task or a session once however often, and of what came to need you only
what still does. You were away since you last quit the TUI (crystal keeps the latest event you had seen in its
database), while your terminal didn't have focus for five minutes or more, or, in a terminal that doesn't say
when it has focus, while you didn't type or click for that long. The line stays until your next key: `a` opens
the timeline with what's new since marked `•`, and `U` lists what needs you.
