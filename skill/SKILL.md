---
name: crystal
description: Run other coding agents in parallel with crystal, hand them work, wait for them, read their answers and answer their questions. Use when a task splits into parts other agents can do at the same time, when you want a second agent to review or test your change, or when work should happen in its own git worktree. Requires the `crystal` command.
---

# crystal

crystal runs agents (Claude Code, Codex, or any terminal program) in sessions that a background daemon
keeps alive. Every session has a name. Inside a session, crystal commands reach the same daemon on their own.

## Start an agent

```sh
crystal new -d -n reviewer claude "Review the diff on this branch for bugs"
crystal new -d -n fixer -w fix/login claude "Fix the login redirect bug"
```

- `-d` starts it in the background and prints its name. Always pass it: without it, crystal attaches.
- `-n` names it. Without it the name comes from its prompt, like `review-diff-branch`, or else is the
  program's, with `-2`, `-3` added if taken. Use the name `-d` prints.
- `-w <branch>` starts it in a new git worktree, `<repo>.worktrees/<branch>` (or under the user's
  `[worktrees] directory`), making the branch if it doesn't exist. Use it when the agent will edit files, so it doesn't collide with you. A new branch starts
  from origin's default branch, freshly fetched; add `--base HEAD` when it should have your commits, say to
  review or test your change (commit first: uncommitted work doesn't come along).
- Several things at once, like a few fixes the user asked for together, are a session each, with `-w`: the
  user sees each in the sidebar, with its status, its diff and its screen, and can step in. Don't use the
  Agent tool's worktree isolation or `EnterWorktree` for them: those worktrees are yours alone, and nothing
  shows until one is left behind.
- Words after `claude` are its first prompt. Quote them as one argument.

## Work in a worktree yourself

When the user asks you to do the work in a worktree, have crystal move your session into one rather than
entering one of your own:

```sh
crystal worktree move fix/login
```

It takes the project's worktree on that branch, or makes one (a made-up branch with no name given; `--base
HEAD` to start from your commits). Run it once, then end your turn at once, saying in a line that you're
moving: crystal stops you when the turn ends and starts you again in the worktree, in this conversation, with
a prompt to carry on there. If it fails, say why and carry on where you are. `-n <name>` moves another session,
a background task too, once its run ends.

`crystal worktree list` shows the project's worktrees and the sessions in each; `crystal worktree create
<branch>` makes one and prints its directory (`--label "what it's for"` names it in the sidebar).

## Hand it work and wait

```sh
crystal send reviewer "Now check the tests too" --wait
```

`--wait` waits for the turn that text starts, then prints how it ended. Then read the answer:

```sh
crystal read reviewer --lines 40
```

`read` prints the screen as text. `--lines N` keeps the last N rows that aren't blank. `--history` adds what
has scrolled off the screen. `--since 10m` keeps only what it wrote in the last ten minutes, and `--unwrap`
joins a long line the screen wrapped. A long message goes in from a file or a pipe: `crystal send reviewer -
< notes.md`.

## Or run a task

For work that needs no back-and-forth, a task runs Claude without a terminal (`claude -p`) and keeps its
answer apart from the screen:

```sh
crystal task --wait -n tests "Run the tests and fix what fails" -- --permission-mode acceptEdits
crystal result tests
crystal send tests "Now add a test for the bug" --wait
crystal result tests
```

- A task that asks for a permission waits: `wait` prints `waiting`, and `crystal tasks show <name>` says what
  it asks for. The user answers it; answer it yourself (`crystal answer <name> y`, `n -m "<why>"`, or
  `always`) only when the user has said what to allow. Better, allow what it needs up front, after `--`,
  with `--allowedTools` or `--permission-mode`.
- `crystal interrupt <name>` stops its run; the task stays open, and a follow-up carries on.
- A task takes follow-ups with `send`, one at a time, never `send-keys`.
- `--accept "<criterion>"`, once for each, tells it what has to hold before it's done. `--pr <number>` runs it
  in that pull request's worktree and `--issue <number>` says which issue it's for; with either, the prompt
  can be left out.
- How its run ends closes its task: `task --wait` prints `done` or `failed`, and `result` says why. Either
  way it stays for a follow-up, which carries the conversation on. One whose `claude` crashed has ended:
  `wait` prints `exited N`. Past the user's daily budget, a new task or follow-up is refused, saying so: tell
  the user rather than retrying.
- `crystal tasks terminal <name>` turns it into Claude Code in a terminal, in its conversation: only when the
  user asks.

## Or run a flow

A flow is a chain of tasks on one goal, set up in the user's config file (`crystal config` shows it) or the
project's `.crystal/flows.toml`, like plan, then implement in a worktree, then a review the user approves. Run
one when the user asks for it by name; `crystal flow defs` lists them:

```sh
crystal flow run ship "Retry the webhook when it times out"   # prints the run's name, like ship-1
crystal flow wait ship-1 --timeout 1800                        # until a gate, the end, or a failure
crystal flow show ship-1                                       # each step and the first line of its answer
crystal result ship-1-review                                   # a step's whole answer: its session is <run>-<step>
```

- `flow wait` prints `waiting at <step>` when the run stops at a gate for the user, or `done`. It fails, saying
  `failed at <step>: <why>`, when a step fails.
- A gate is the user's to answer. Go on past it (`crystal flow approve <run>`) or send it back with notes
  (`crystal flow back <run> "<notes>"`) only when the user tells you to.
- `crystal flow retry <run>` runs a failed or interrupted step again. `crystal flow --json` lists every run.
- `crystal flow cancel <run>` cancels a run and its step's open task: only when the user asks.
- A step on an agent other than Claude, or on Claude with `background = false`, runs in a terminal session,
  `<run>-<step>`, and the flow goes on once its task closes: `crystal read` it rather than `result`.
- With flows turned off, these commands say "the flows plugin is off"; run the steps as tasks yourself.

## Close your task

A session started with something to do is a task, and stays open until it's closed. If you were started
with a task, close it when you're through, with one line on how it went:

```sh
crystal done "Fixed the redirect and added a test"
crystal done --failed "The staging database is down"
crystal done "Wrote the plan" --artifact docs/plan.md
```

- `crystal done` closes the task of the session it runs in. `-n <name>` closes another's.
- `--artifact <path>`, once for each file, keeps a copy of a file in your worktree with the task, for whoever
  reads it once the worktree is gone: a plan, a report. A file can be 1 MiB at most. One that can't be kept
  refuses the close and says why; the task stays open, so fix the call and run it again.
- `crystal done` refuses while your worktree is in the middle of a rebase or a merge, stopped on conflicts
  say: the work isn't done then. Finish it or abort it, then close the task; `--failed` closes it anyway.
- End a turn with your task still open and crystal reminds you, once. Close it then if you're through;
  if you're waiting on the user, leave it open and end your turn.
- An agent you start with a prompt (`crystal new -d claude "…"`, or `-t "…"` for any command) is given a task.
  `crystal ls --json` shows it: `task.id`, `task.goal`, and once closed, `task.outcome` with `failed`,
  `cancelled` and `summary`.
- A background task (`crystal task`) closes itself when its run ends, done or failed, as Claude's answer
  says; a follow-up opens it again.
- Each task has a number, like `t12`. `crystal tasks` lists the project's tasks with theirs and how each
  stands: `pending`, `running`, `waiting` (its turn ended with it open: it's asking the user), `done`,
  `failed` or `cancelled`. `crystal tasks show <task>` shows one, `crystal tasks log <task>` adds its
  transcript, and `crystal tasks cancel <task>` cancels it and stops its session: only when the user asks.
- `crystal tasks new "<goal>"` makes a task and starts an agent on it, printing its number; `--background`
  runs it as `crystal task` does, and `--no-launch` leaves it pending until `crystal tasks start <task>`.
  `--accept`, `--pr` and `--issue` work as for `crystal task`.
- Given acceptance criteria, close your task done only once each holds.

## Leave notes for the next session

Each git worktree keeps notes for the sessions that work in it after you, in `.crystal/handoff.md`. When
you're told it has some, read that file before you start. When you learn something the next session there
should know, like a decision and why, a dead end, a command that works or what you left undone, add a note:

```sh
crystal handoff "The fixtures live in tests/fixtures; cargo test codec runs just them"
```

- Keep each note short and whole on its own; crystal adds the time and your session's name. Never edit the
  file yourself.
- Closing your task adds its summary there for you.
- With the handoff plugin off, `crystal handoff` says so; carry on without it.

## Keep a backlog

Each project has a backlog of things to do later. When you notice something worth doing that isn't part of
your task, put it there rather than into your change:

```sh
crystal backlog add "Retry the webhook on a timeout" -t payments   # prints its number, like #4
crystal backlog add "Retry the webhook" -b "On a timeout only, not a 4xx."   # more than a line: a body
crystal backlog                   # what's to do; -t payments for one tag's
crystal backlog show 4            # one item: its body, and the tasks started for it and how they went
crystal backlog edit 4 -b "Twice, then give up"   # its line, -b its body, -t its tags
crystal backlog done 4            # tick it off; reopen 4 or rm 4 undo it
crystal backlog start 4 -w -d     # an agent on #4 in a new worktree; closing its task done ticks #4
```

Every command works on the current directory's project; `-C <dir>` names another. `backlog start` takes `-p
<profile>`, `--pr <number>` to work in a pull request's worktree, and `--background` for `claude -p`.

## Statuses

`crystal wait <name>`, `send --wait` and `send-keys --wait` print one of these:

| Status | Meaning | What to do |
|---|---|---|
| `done` | finished its turn | `read` the answer |
| `idle` | at its prompt, already seen | `read`, or `send` more work |
| `waiting` | asking something: a permission, a choice, or a question it ended its turn on with its task open | `read` it, then answer: a permission or a choice with `send-keys` (a background task's permission with `crystal answer`), a question with `send` |
| `exited N`, `killed (…)` | the program ended | `read` its last screen; `crystal respawn <name>` runs it again |
| `couldn't start` | it couldn't start again after crystal restarted: its directory or its command has gone | `crystal ls` says why; once that's put right, `crystal respawn <name>` starts it |

To wait for one status in particular, or for a program to print something:

```sh
crystal wait reviewer --until waiting --timeout 600   # or working, done, idle, ended; several with commas
crystal wait server --output 'listening on' --timeout 60   # a regular expression; prints the line
```

- A wait that runs out of time exits 2; anything else that goes wrong exits 1. `--quiet` prints nothing.
- `--until` fails when the program ends first, unless `ended` is one it waits for.
- A turn that ends while the user watches it is `idle` at once, never `done`: wait for `done,idle`.
- `--output` counts what's on the screen already, and the rows just above it.
- `crystal events -n reviewer` prints what happened to a session, one line each; `--follow` keeps printing.

## Answer its questions

When an agent asks for a permission or offers numbered choices, its status is `waiting`. Read the question,
then press the keys a person would:

```sh
crystal read reviewer --lines 20
crystal send-keys reviewer 1 --wait
```

`send-keys` takes key names (`Enter`, `Escape`, `Tab`, `Up`, `Down`, `BSpace`, `C-c`) or text, typed as
keys. `Escape` declines most questions.

## See every session

```sh
crystal ls --json
```

One object per session: `name`, `status` (the word `crystal ls` shows), `state`, `activity`, `cwd`,
`command`, `worktree` with `project`, `branch` and `path` when it's in a git repository, `front`: what's in
front in its terminal, `{"kind": "agent", "program": "claude", …}`, a `shell` or another `program`, and `task`
when it was started with something to do.

## Show a session beside yours

The user watches sessions in crystal's TUI. To put one you started on their screen, beside your own pane:

```sh
crystal pane split tests          # to the right of your pane; --down below it
crystal pane close tests          # off the screen again
crystal layout --json             # the TUI's tabs, the sessions in each, and how their panes split
```

- They go to the TUI the user used last. With none open, the daemon lays the tabs out itself, and the TUI
  opens on them.
- They change what the user sees. Split off what helps them follow your work, close it when it's done, and
  leave their tabs and focus alone unless they ask: `crystal pane focus <name>` hands a session their
  keyboard, and `crystal tab new <name>` brings a new tab to the front, where sessions started after go.

## Tell the user

```sh
crystal notify "The migration is ready for you to review"
```

A desktop notification, as crystal sends when a session needs the user; clicking it takes them to your session
(`-n <name>` for another). Use it only for what needs them now and they might miss: a question asked, a long
job finished. Your status says `waiting` or `done` without it.

## Clean up

```sh
crystal kill reviewer
crystal worktree rm fix/login
```

`worktree rm` refuses while a session still runs there, and when the worktree has uncommitted changes.
`--force` removes it with them, and they're lost: use it only when the user says so. `crystal kill reviewer
--remove-worktree` takes the worktree it was the last session in with it, the same way; without it, `kill`
keeps that worktree and says how to remove it.

## Remember what you learned

A project keeps what its sessions learned, and later Claude Code sessions there are shown the parts that fit
their prompt. Add to it when you find something the next session would otherwise have to find again:

```sh
crystal remember -k gotcha -f tests/ledger.rs "The ledger tests need the database up: make db"
crystal remember -k decision --title "Fees are kept in cents" "A float loses a cent in a refund."
crystal memory search ledger
crystal memory search ledger -k gotcha -f tests   # of a kind, about the files in tests/
```

- `-k` is `decision`, `gotcha`, `command` or `note` (the default). Keep each entry to a sentence or two.
- `-f <file>` names a file the entry is about. Once some of its files change, the entry is marked drifting, and
  once all of them have, stale.
- Don't remember what the code, the git log or CLAUDE.md already says, or anything that only matters now.
- `crystal memory search` matches any of its words, or a word they start or stem from, best first; with
  search by meaning on, entries that mean the same count too, so a few plain words do. It leaves out stale
  entries; `--all` brings them back.
- An entry's first line is what lists show of it; `--title` gives it one of its own, over the rest.
- In a Claude Code session crystal started, the `memory_search` and `memory_show` tools search the memory and
  read an entry by its id without a shell command; use them when you have them.
- `crystal memory` lists every entry; `crystal memory show <id>` reads one in full; `crystal memory rm <id>`
  forgets one that's wrong, and `crystal memory list --forgotten` lists what was.
- Once your task closes, a model reads what you did and keeps what it finds worth keeping, so remember what
  only you know, like why you chose something; it never repeats what's there already.
- With memory turned off, these commands say "the memory plugin is off"; carry on without them.

## Pitfalls

- `send` pastes its text. Agents ignore a pasted answer to a question: answer with `send-keys`.
- `wait` returns at once when the session is already `done`, `idle` or `waiting`. To wait for the turn your
  input starts, use `send --wait` or `send-keys --wait`, not `send` then `wait`.
- A program that doesn't report what it's doing (a shell, a build) counts as busy until it exits: `wait`
  blocks until then. Pass `--timeout <seconds>`; it exits 2 when the time runs out.
- `send` refuses an agent that's asking the user something, with an error starting `agent_blocked:`: the
  text would land in its question. Answer it with `send-keys` (`crystal answer` for a background task) if
  that's yours to answer, or leave it to the user; `--force` types it anyway.
- What you `send` another session starts with a line saying it's from yours, and what you work on. Send 20 a
  minute at most: past that it's refused, as two agents answering each other are probably in a loop; stop
  sending and carry on with your own work. Never send only to say you got a message: a message that only
  acknowledges, like `ok` or `thanks!`, is refused.
- A background task works on one prompt at a time: `send --interrupt` stops its run and sends yours in its
  place. An agent in a terminal is stopped with `send-keys <name> Escape`.
- Don't `kill` or `send` to your own session. `$CRYSTAL_SESSION` is its name when it started.
