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
`command`, and `worktree` with `project`, `branch` and `path` when it's in a git repository.

## Clean up

```sh
crystal kill reviewer
crystal worktree rm fix/login
```

`worktree rm` refuses while a session still runs there, and leaves uncommitted changes to git's own refusal.

## Pitfalls

- `send` pastes its text. Agents ignore a pasted answer to a question: answer with `send-keys`.
- `wait` returns at once when the session is already `done`, `idle` or `waiting`. To wait for the turn your
  input starts, use `send --wait` or `send-keys --wait`, not `send` then `wait`.
- A program that doesn't report what it's doing (a shell, a build) counts as busy until it exits: `wait`
  blocks until then. Pass `--timeout <seconds>`; it fails when the time runs out.
- Don't `kill` or `send` to your own session. `$CRYSTAL_SESSION` is its name when it started.
