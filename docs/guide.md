# The crystal guide

crystal runs your coding agents in a daemon that outlives the window, and lists every session in one
sidebar. You don't have to remember a key: `?` shows every one, `:` lists every command by its name, and a
right click on anything offers what it does. This page is `?` and then `Tab`, `guide` in the command list, or
`crystal guide` in a shell.

## What to start

`n` opens the new-session panel: type what the agent should do, choose what runs it with `←` and `→`, and
`Enter` starts it. `w` does the same in a new worktree, on a branch of its own.

| Start | When it fits | What you get back |
|---|---|---|
| **A session** | You want to drive: talk it through, steer, read along | A live terminal; `Enter` types into it |
| **A task** | Something it can finish alone: give it something to do | A session marked open until its agent runs `crystal done`, or you close it with `c` |
| **A background task** | Claude Code, unattended: the panel's "how" row, "in the background" | Its transcript in the pane, a cost cap, and its answer as the summary; `C` opens it in a terminal |

`t` is a new tab with a shell in it. `P` keeps profiles: saved ways of starting an agent, with its model,
effort and mode, text before and after the task, where it starts, and as a session, a task or a background
task.

## Flows, the backlog and memory

**Flows** chain tasks on one goal, a step each, some waiting at a gate for you: `g` goes on, `f` sends it back
with notes. Start one from the panel (`flow: <name>`) or with `crystal flow run <name> "<goal>"`.

**The backlog** is the project's list of things to come back to: `b` opens it, and `Enter` on an item hands it
to an agent as a task that ticks it when it's done. Agents add to it with `crystal backlog add`.

**Memory** is what sessions learned about the project, shown to every agent started there when it matters,
and to Claude Code as it first reads or edits a file, the few lines about that file it hasn't seen (`[memory]
recall_on_read` turns that off). `m` reads it; agents write it with `crystal remember`.

## When something needs you

A session waiting on you wears `▲` and is pinned at the top of the sidebar, in whichever tab it's in. So is
one a crash or a reboot couldn't start again, saying why: put that right and `Enter` starts it.

- `u` goes to the next one that needs you, and `U` lists everything that does, the most urgent first.
- `y`, `n` and `Y` answer a background task asking for a permission: yes, no, always.
- `Space` replies without going into the pane; `Enter` types into it.
- `a` is the timeline: what happened, as it happens, and what did while you were away.

## Fifteen keys

| Key | What it does |
|---|---|
| `j` `k` | Move down and up the sidebar |
| `Enter` | Type into the selected session |
| `Ctrl+\` | Leave a pane: the keyboard goes back to the sidebar |
| `Ctrl+B` | The prefix: in a pane, the next key is the sidebar's |
| `n` `w` | A new session, or one in a new worktree |
| `Space` | Reply to the selected session |
| `u` | The next session that needs you |
| `/` | Find a session, a worktree, a flow run, a pull request, an issue or a backlog item |
| `:` | Every command, by its name |
| `\|` `-` | Split the pane, side by side or one above the other |
| `z` | Zoom the pane to the whole screen, and back |
| `d` | What changed in the session's worktree |
| `p` | Find a file in the worktree, and edit it |
| `?` | Every key, and this guide |
| `q` | Quit; the sessions keep running |

These are the keys as they come: `[keys]` in the config file changes any of them, and `?` shows yours.

## What agents call

An agent in a session runs these; you can too.

| Command | Example |
|---|---|
| `crystal done` | `crystal done "token store on sqlite, 42 tests pass"` |
| `crystal done --failed` | `crystal done --failed "the migration needs a schema decision"` |
| `crystal send` | `crystal send reviewer "the fix is pushed, look again"` |
| `crystal wait` | `crystal wait reviewer --until idle,done --timeout 1800` |
| `crystal read` | `crystal read reviewer --lines 40` |
| `crystal task` | `crystal task -w changelog "write the changelog entry"` |
| `crystal backlog add` | `crystal backlog add "flaky test in auth::refresh"` |
| `crystal remember` | `crystal remember -k command "run the tests with --test-threads=4"` |
| `crystal handoff` | `crystal handoff "the schema change is half done; see migrations/003"` |
| `crystal open` | `crystal open docs/explain-hooks.md`, when you ask to see it |

## Where things live

| What | Where |
|---|---|
| Settings | `~/.config/crystal/config.toml`: `crystal config` prints them, `,` changes the common ones |
| Your agent rules | `agents/` beside the config file |
| Sessions, tabs, tasks, the backlog, events | `crystal.db` in `~/.local/state/crystal` |
| What sessions learned | `memory.db` beside it; `m` reads it |
| Screenshots dropped on a task or a reply | `attachments/` beside it, for a week |
| Notes for the next session in a worktree | `.crystal/handoff.md` at its top |
| A project's run and open commands, its flows | `.crystal/project.toml` and `.crystal/flows.toml` in it |
