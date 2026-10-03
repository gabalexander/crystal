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
- `-n` names it. Without it the name is the program's, with `-2`, `-3` added if taken.
- `-w <branch>` starts it in a new git worktree, `<repo>.worktrees/<branch>`, making the branch if it
  doesn't exist. Use it when the agent will edit files, so it doesn't collide with you.
- Words after `claude` are its first prompt. Quote them as one argument.

## Hand it work and wait

```sh
crystal send reviewer "Now check the tests too" --wait
```

`--wait` waits for the turn that text starts, then prints how it ended. Then read the answer:

```sh
crystal read reviewer --lines 40
```

`read` prints the screen as text. `--lines N` keeps the last N rows that aren't blank. `--history` adds what
has scrolled off the screen.

## Or run a task

For work that needs no back-and-forth, a task runs Claude without a terminal (`claude -p`) and keeps its
answer apart from the screen:

```sh
crystal task --wait -n tests "Run the tests and fix what fails" -- --permission-mode acceptEdits
crystal result tests
crystal send tests "Now add a test for the bug" --wait
crystal result tests
```

- Nobody can approve a permission for a task: allow what it needs after `--`, with `--allowedTools` or
  `--permission-mode`. What it was refused shows at the end of its transcript (`crystal read`).
- A task takes follow-ups with `send`, one at a time, never `send-keys`.
- A task whose run fails ends: `wait` prints `exited N`, and `result` says why.

## Or run a flow

A flow is a chain of tasks on one goal, set up in the user's config file (`crystal config` shows it), like
plan, then implement in a worktree, then a review the user approves. Run one when the user asks for it by
name:

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
- With flows turned off, these commands say "the flows plugin is off"; run the steps as tasks yourself.

## Close your task

A session started with something to do is a task, and stays open until it's closed. If you were started
with a task, close it when you're through, with one line on how it went:

```sh
crystal done "Fixed the redirect and added a test"
crystal done --failed "The staging database is down"
```

- `crystal done` closes the task of the session it runs in. `-n <name>` closes another's.
- End a turn with your task still open and crystal reminds you, once. Close it then if you're through;
  if you're waiting on the user, leave it open and end your turn.
- An agent you start with a prompt (`crystal new -d claude "…"`, or `-t "…"` for any command) is given a task.
  `crystal ls --json` shows it: `task.goal`, and once closed, `task.outcome` with `failed` and `summary`.
- A background task (`crystal task`) closes itself when its run ends.
- `crystal tasks` lists the project's tasks: open ones, then those closed, the latest first.

## Keep a backlog

Each project has a backlog of things to do later. When you notice something worth doing that isn't part of
your task, put it there rather than into your change:

```sh
crystal backlog add "Retry the webhook on a timeout" -t payments   # prints its number, like #4
crystal backlog                   # what's to do
crystal backlog done 4            # tick it off; reopen 4 or rm 4 undo it
crystal backlog start 4 -w -d     # an agent on #4 in a new worktree; closing its task done ticks #4
```

Every command works on the current directory's project; `-C <dir>` names another.

## Statuses

`crystal wait <name>`, `send --wait` and `send-keys --wait` print one of these:

| Status | Meaning | What to do |
|---|---|---|
| `done` | finished its turn | `read` the answer |
| `idle` | at its prompt, already seen | `read`, or `send` more work |
| `waiting` | asking something: a permission, a choice | `read` the question, then answer with `send-keys` |
| `exited N`, `killed (…)` | the program ended | `read` its last screen; `crystal respawn <name>` runs it again |

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

## Clean up

```sh
crystal kill reviewer
crystal worktree rm fix/login
```

`worktree rm` refuses while a session still runs there, and leaves uncommitted changes to git's own refusal.

## Remember what you learned

A project keeps what its sessions learned, and later Claude Code sessions there are shown the parts that fit
their prompt. Add to it when you find something the next session would otherwise have to find again:

```sh
crystal remember -k gotcha -f tests/ledger.rs "The ledger tests need the database up: make db"
crystal memory search ledger
```

- `-k` is `decision`, `gotcha`, `command` or `note` (the default). Keep each entry to a sentence or two.
- `-f <file>` names a file the entry is about. When that file changes, the entry is marked stale.
- Don't remember what the code, the git log or CLAUDE.md already says, or anything that only matters now.
- `crystal memory` lists every entry; `crystal memory rm <id>` forgets one that's wrong.
- With memory turned off, these commands say "the memory plugin is off"; carry on without them.

## Pitfalls

- `send` pastes its text. Agents ignore a pasted answer to a question: answer with `send-keys`.
- `wait` returns at once when the session is already `done`, `idle` or `waiting`. To wait for the turn your
  input starts, use `send --wait` or `send-keys --wait`, not `send` then `wait`.
- A program that doesn't report what it's doing (a shell, a build) counts as busy until it exits: `wait`
  blocks until then. Pass `--timeout <seconds>`; it fails when the time runs out.
- Don't `kill` or `send` to your own session. `$CRYSTAL_SESSION` is its name when it started.
