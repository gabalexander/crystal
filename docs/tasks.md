# Tasks and the backlog

<sub>[← README](../README.md#documentation)</sub>

A task is a session started with something to do, which stays open until it's closed, done or failed. This
page covers tasks, the handoff notes and files they leave, background tasks (Claude Code without a terminal),
and each project's backlog of work for later.

- [Tasks](#tasks)
  - [The handoff file](#the-handoff-file)
  - [Kept files](#kept-files)
- [Background tasks](#background-tasks)
- [The backlog](#the-backlog)

## Tasks

A session started with something to do is a task: an agent given a task in the new-session panel, `crystal
new -t "<task>" claude` (or simply `crystal new claude "<task>"`), a [background task](#background-tasks), one
started from the [backlog](#the-backlog), or one made with `crystal tasks new`. Each task is numbered as it's
made, `t1`, `t2`…, and stays open until it's closed, done or failed, with a line on how it went:

- The agent closes it from inside its session: `crystal done "<what was done>"`, or `crystal done --failed
  "<why>"`. crystal tells Claude Code how, on top of its system prompt, Codex in its developer instructions,
  and other agents at the top of their first prompt, opening with a line on where that comes from, so the
  agent doesn't take it for a stranger's instructions. Agents don't always remember to, Haiku least of all, so
  the first time Claude Code ends a turn with its task still open, saying nothing that makes clear why, its Stop
  hook reminds it and it carries on: to close the task if it's done, to ask you its question plainly if it needs
  you, or to say what work of its own it waits on, which wakes it, and end its turn. `-n <session>` closes another
  session's task. A task isn't done while its worktree is in the middle of a rebase or a merge, stopped on
  conflicts say: `crystal done` refuses then and says so, until the agent finishes or aborts it; `--failed`
  closes it anyway.
- You close it from the TUI: `c` on the session asks `d` done or `f` failed, then for a line on how it went,
  which can stay empty.
- A background task closes itself when its run ends, as Claude's result says: done with the first line of
  its answer, or failed. Its session stays either way, and a follow-up opens the task again.

| State | Meaning |
|---|---|
| `pending` | made with `--no-launch`: nothing works on it yet |
| `running` | its session is working on it, or its agent left it open waiting on something other than you |
| `waiting` | its agent's turn ended with the task still open, asking you something |
| `done` | closed done |
| `failed` | closed failed, or its session ended while it was open, leaving nobody who could close it |
| `cancelled` | you cancelled it, or killed its session while it was open |

Whether a turn that ends with the task still open needs you goes by what the agent said last, which Claude
Code's Stop hook gives, read with no model:

- A question, or a request: a choice, a decision, an approval, a file, access, or a command for you to run. The
  session waits on you (`▲`, pinned, `u` finds it, and you're told) until its agent works again, and its task
  line says so. It isn't reminded first: it asked.
- A plain statement that it waits on something else: "waiting on CI, not on you", "I'll pick up when the tests
  finish". The task stays open and the session sits idle, marked `◇`: not pinned, not told of, not found by
  `u` or `U`. With work of its own still running that wakes it, the turn is held as working instead (see
  [what an agent is doing](agents.md#what-an-agent-is-doing)).
- Anything else, like a summary that doesn't close the task, is unclear: it's reminded once, and if its next turn
  ends as unclear, it waits on you. So does a turn whose end its hooks didn't give, from another agent or an
  older Claude Code.

Of the 27 turns crystal's own log had marked as waiting on the user, none asked anything: 25 waited on their
tests, CI, a helper or a subagent, and two were one finished report the agent didn't close. The rule reads 24 of
them as waiting on something else, and 25 had work of their own running that holds them; together they leave
out 25, and the finished report still tells you. Over the 472 turn ends a person answered next, it took none
that asked for one that didn't.

A background task you interrupted waits on you too. A program that exits with its task open fails it, saying how it ended; `crystal
respawn` opens it again, under the same number.

The sidebar shows a task under its session: what it was asked to do while it's open, `▲` when it waits on you,
`⚠` and the permission a background task asks for, and `✓`, `✗` or `–` with how it went once it's closed. So
does the pane's header. `crystal ls` has a TASK column, and `ls --json` a `task` field.

```sh
crystal tasks                                       # the project's tasks: open, waiting to start, then closed
crystal tasks new "Fix the flaky test"              # the new-session panel's first agent on it; prints t12
crystal tasks new --background "Bump the deps" -- --model opus   # or Claude in the background
crystal tasks new --accept "cargo test passes" "Fix the flaky test"   # with what has to hold
crystal tasks new --pr 57 --background              # on pull request 57, in its worktree
crystal tasks new --issue 7                         # to fix issue 7
crystal tasks new --no-launch "Tidy the README"     # made now, started later: prints t13
crystal tasks start t13                             # start it; prints its session's name
crystal tasks show t12                              # how it stands, its session, what it asks for and costs
crystal wait t12                                    # until it closes; prints done, failed or cancelled
crystal tasks cancel t12                            # cancel it, and stop its session
crystal tasks log t12                               # how it stands, then its session's transcript
crystal tasks terminal t12                          # a background task, opened in a terminal
```

`tasks new` works from a shell or from inside a session, in the current directory (`-c <dir>`, or `-w
<branch>` for a new worktree, made at once), and `-n` names its session. A task is named by its number, with
or without the `t`, or by its session's name. `crystal task <prompt>` still starts a background task;
`crystal tasks` is about the tasks there are. `tasks terminal` opens a [background task](#background-tasks) in a
terminal, the tasks plugin on or off.

A task can carry acceptance criteria, what has to hold before it's done: `--accept "<criterion>"`, once for
each, or `--accept-file <file>`, a line each (a list's `-` or `[ ]` taken off, and `#` headings left out), on
`crystal task` or `crystal tasks new`. Its agent is given them in its first prompt, under its goal
(`Acceptance criteria:`), so they're on its screen, a background task's transcript too. `crystal tasks show`
lists them, and `ls --json` has them as `task.accept`. A task made to wait keeps them until it starts.

A task can be about a pull request or an issue of its project's [forge](code.md#pull-requests-and-issues), by its
number. `--pr 57` runs it in that pull request's worktree: the one the project has on its branch, or else a
new one, its commits fetched, a fork's under its owner, as `O` makes it. `--issue 7` says which issue it's for,
wherever it runs. Its agent is told of them by the road its task takes, Claude Code on top of its system
prompt: to read the pull request or the issue and its conversation first (`gh pr view 57 --comments`, or
`glab`'s), to keep to what it needs, and to close the issue from the pull request that fixes it (`Closes
#7`). Given no goal, a task on a pull request is to work on it, and one on an issue to fix it, in the words
`O` and `i` use. `crystal tasks show` names them, and `ls --json` has them as `task.pull_request` and
`task.issue`. Both ask the forge, so they need the github plugin and `gh` or `glab`; a number it doesn't know
starts nothing.

The pull request and the issue stay with the session as standing context, task or not: a session started on
one from `O` or `i` with tasks off, or by a [profile](configuration.md#profiles) that starts a plain session, is told of it the
same way, as what the session is for. Each time its agent starts again, after a crash or a reboot, from the
archive or with `crystal respawn`, in its conversation or afresh, it's told again, so it never loses track of
what it was there to do.

Closed tasks are kept in the project's history in crystal's database: what each was asked, when and how it
closed, and the session and branch it ran in. `crystal tasks` lists the project's tasks, open ones first, then
those waiting to start, then those closed, the latest first; `--all` lists every project's, `-C <dir>` another
project's, and `--json` prints them for scripts. `tasks log` shows a task's transcript while its session is
still in the list.

### The handoff file

A task's summary is one line on what it did. What a worktree learned along the way, the next session there
would otherwise learn again, so each git worktree keeps notes for the sessions after: `.crystal/handoff.md`,
at its top. An agent adds one when it learns something the next session there should know:

```sh
crystal handoff "The fixtures live in tests/fixtures; cargo test codec runs just them"
```

```
## 2026-10-04T14:03:07+02:00 · porter · task "Port the codec"
The fixtures live in tests/fixtures; cargo test codec runs just them

## 2026-10-04T14:41:52+02:00 · porter · task "Port the codec" done
Ported the codec and its tests
```

- Each note is a section: a heading with the local time, the session's name and the task it has open, then the
  note, its spaces and blank lines tidied and cut at 8 KiB. `-n <session>` adds one for another session's
  worktree.
- A task that closes done or failed adds its summary, or why it failed, under a heading that ends in how it
  closed. A cancelled one adds nothing.
- Every agent crystal starts in a worktree whose file has notes is told to read it first and how to add to it,
  with the file's last 2 KiB, by the same road as its task: Claude Code on top of its system prompt, a
  background task too, Codex in its developer instructions, and Gemini CLI, OpenCode, Cursor, Qwen Code and
  Pi at the top of their first prompt. When the file's end would make what it's asked and told more than 16
  KiB, it's told where the file is without it; a first prompt too long even so loses what the memory has
  first, then the notes.
- crystal is the file's only writer. It keeps it to 256 KiB, letting the oldest notes go, with `[earlier
  notes trimmed]` on top.
- The notes stay out of git: the first note writes a `.gitignore` beside them that ignores everything in
  `.crystal/` but `flows.toml` (a `.gitignore` there already is left as it is). A project whose notes should
  travel with its branches lists its main worktree under `[handoff]` in the [settings](configuration.md), and the
  `.gitignore` isn't written:

  ```toml
  [handoff]
  in_git = ["~/code/app"]
  ```

- Notes are a git worktree's: a session outside git has none. `crystal plugin disable handoff` turns them off.
- `M` in the TUI reads them, with the selected session's [kept files](#kept-files): see below.

### Kept files

`crystal done "<summary>" --artifact <path>` keeps a copy of a file with the task as it closes: a plan, a
report, a screenshot, whatever someone will want once the worktree is gone. `--artifact` can be given more
than once.

- Each must be a file in the session's worktree, not a link or a directory, of 1 MiB at most, and 8 MiB for all
  of them. A path is taken from the directory the command runs in. One that can't be kept refuses the whole
  close, saying why, and the task stays open, so the agent can fix the call and run it again.
- The daemon copies them itself, after checking each again, into its server's state directory, beside its
  database: `~/.local/state/crystal/tasks/t12/` for the default server. Two files with one name are kept as
  `plan.md` and `plan-2.md`.
- A task that closes done or failed keeps its worktree's handoff file too, as it is then, as `handoff.md`.
- `crystal tasks show t12` lists them, and `crystal tasks --json` gives each task's `artifacts`, with their
  `kind` (`file` or `handoff`), `name`, `path` and `bytes`. Each is a `task.artifact` in the [event
  log](events.md).

`M` in the TUI, or **its handoff notes and files** in a session's right-click menu, shows what the selected
session leaves for the next: its worktree's handoff file as it is now, then the files its task kept, the
handoff file as it was when the task closed among them. The list is on the left, each file with what it is
and its size, and the one the bar is on is read on the right, a markdown file as its page (`Ctrl+R` flips it
to its source). `↑`/`↓` (or `j`/`k`) choose, `PgUp`, `PgDn` and `Space` scroll, `Enter` opens the file in
your `$EDITOR`, as [the file finder](code.md#the-file-finder-and-the-tree-browser) does, and `Esc` closes. With the
handoff plugin off it shows only the kept files, and with tasks off only the notes.

## Background tasks

A task is Claude Code without a terminal: `claude -p`, running a prompt in the background. It sits in the
session list like any session, with a transcript you can watch in the TUI, attach to, or `read`: the prompt,
what Claude says, laid out as [markdown](code.md#the-file-finder-and-the-tree-browser), each tool it uses with the first
line of what came back, the permissions it asks for and how you answered, and how each run ended, with how long
it took and what it cost.

```sh
crystal task -n docs "Update the README for the new flags"           # prints the task's name
crystal task --wait -n tests "Run the tests and fix what fails" -- --permission-mode acceptEdits
crystal task --accept "cargo test passes" "Fix the flaky test"       # with what has to hold before it's done
crystal task --pr 57                                                 # work on pull request 57, in its worktree
crystal result tests                                                 # Claude's answer at the end of the run
crystal send docs "Now the changelog too" --wait                     # a follow-up, in the same conversation
crystal send --interrupt docs "Leave the tests alone"               # stop the run, and carry on from there
crystal answer docs y                                                # allow what it asks for: y, n or always
crystal interrupt docs                                               # stop the run it's in the middle of
crystal tasks terminal docs                                          # carry on with it yourself, in a terminal
```

- One `claude` takes the task's prompt and each follow-up after it, a run each, over its standard input
  (`--input-format stream-json`). Arguments after `--` go to every `claude -p` the task starts.
- `--wait` waits for the run to end and prints how it went: `done` or `failed`, as Claude's answer says, or
  `waiting` when it stops to ask for a permission first.
- `--accept`, `--pr` and `--issue` give a task acceptance criteria, and a pull request or an issue to work on:
  see [tasks](#tasks).
- When Claude asks for a tool its permission mode and rules don't allow, the run waits on you: the session
  shows as `waiting`, its transcript and its pane's header say what it asks (`⚠ Bash cargo test`), and `ls
  --json` has it as `asking`. `y` in the TUI, on the task in the sidebar, in its pane or in the list `U` opens, or
  `crystal answer <task> y`, lets it run once. `n` says no: Claude is told so, or what `-m` says, and carries on. `Y`, or
  `always`, lets it run and keeps a rule for calls like it, so they aren't asked about again: for a shell
  command its first word, or its first two for `git`, `cargo`, `npm`, `go` and the like (`Bash(cargo
  test:*)`), and for any other tool the tool. Claude adds the rule to the checkout's
  `.claude/settings.local.json`, so later sessions there have it too. To be asked less to begin with, allow
  what it needs with `--allowedTools` or `--permission-mode` after `--`, or for every task, under `[tasks]` in
  the [settings](configuration.md): `permission_mode`, the mode each `claude -p` starts in (`acceptEdits`, `auto`,
  `dontAsk` or `plan`; `default` passes none, and Claude asks for what isn't allowed), and `allowed_tools`,
  rules like `"Bash(cargo test:*)"` allowed beside crystal's own. A task's own arguments win. They're read as
  each `claude` starts, so a change counts from its next. `bypassPermissions`, which turns every check off for a
  task nobody watches, is refused unless `allow_bypass = true` says so too. The [settings
  view](configuration.md#the-settings-view) goes through the modes.
- Every Claude Code session crystal starts, a task or in a terminal, may run the crystal commands it's told
  to without asking, so one driving others doesn't stop at every step:
  - starting and driving sessions: `ls`, `new`, `send`, `wait`, `read`, `result`, `interrupt`, `events`,
    `rename`, `report`, `notify`, `layout` and `layout export`, `pane split` and `pane close`, and `open`
  - with tasks on, `done`, `task`, and `tasks` with `show`, `log`, `new` and `start`; with flows on, `flow`
    with `run`, `wait`, `show`, `defs` and `retry`
  - with the backlog on, reading it and `add`, `list`, `show`, `edit`, `export`, `done`, `reopen` and
    `start`; with the handoff file on, `handoff`; with memory on, `remember`, and `memory` with `add`,
    `list`, `search` and `show`

  What removes or cancels what's there (`kill`, `worktree rm`, `tasks cancel`, `flow cancel`, `backlog rm`,
  `memory rm`), what writes in bulk from a file (`backlog import`), what's yours to decide (a flow's gate:
  `flow approve` and `back`), and what answers another agent's question for it (`send-keys`, and `answer`,
  which can say yes to a permission) still ask.
- `Ctrl+C` in a task's pane, or `crystal interrupt <task>`, stops the run it's in the middle of. Its task
  stays open, waiting on you, and a follow-up carries on.
- `crystal send`, or `Space` in the TUI, gives a task a follow-up: on the `claude` still there, or once that
  has gone, after five minutes with nothing to do or after a restart, on a new one that carries the
  conversation on with `--resume`. One run at a time: a follow-up sent while Claude is still working is
  refused, unless `send --interrupt` stops the run first, and so is one sent while it asks for a permission. A
  task takes no keys, so `send-keys` is refused too.
- `crystal result <task>` prints the last answer; `--json` adds whether the run failed, the conversation's id,
  the cost so far and how many runs the task has had.
- Each task's `claude` is given `--max-budget-usd`: $5, unless `max_budget_usd` under `[tasks]` in the
  [settings](configuration.md) says otherwise, and `0` for none. A run that reaches it fails. What every task spends
  is added up by the day, and the TUI's footer shows it: `$4.12 today`. With `daily_budget_usd` set, past it
  the footer turns red (`$6.40 today · over $5.00`) and no new run starts until the next day, whether a new
  task, a follow-up or a flow's step: each is refused, saying why. Runs already going carry on.
- How a run ends closes its [task](#tasks), as Claude's own result says: done with the first line of its
  answer, or failed with what went wrong, like `error max turns` or a budget reached. The task stays at rest
  either way, for a follow-up: `crystal send` opens its task again and carries the conversation on, after a
  failure on a new `claude`, with a budget of its own. A `claude` that crashes before saying how its run
  ended ends the task's session, which shows how it exited and why; `crystal respawn` runs its prompt again,
  in its conversation if it got that far.
- A task's pane's header says how full its conversation is, `ctx 12%`: the tokens Claude was given for its
  last message, of the most its model takes, which each run's result says (200,000 until one has, or a
  million for a model run with `[1m]`). `crystal tasks show` says it in tokens, `24k of 200k tokens, 12%`,
  and `ls --json` has it as `context`.
- `C` in the TUI, on the task in the sidebar or in its menu, or `crystal tasks terminal <task>`, opens it in a
  terminal: Claude Code picks its conversation up there (`claude --resume <id>`), in the task's directory,
  under its name and in its place in the list, with the task's own arguments but those only `claude -p` takes
  (`--max-budget-usd`, `--max-turns`, `--output-format` and the like). Its task goes with it as it stood,
  under its number: one still open is closed with `crystal done` from then on, as an agent's in a terminal
  is. There's no way back to the background. It's refused while a run is going on: `crystal wait` for it, or
  `crystal interrupt` it, first.
- `crystal worktree move <branch> -n <task>`, or the task itself running it in a run, moves it into another
  worktree of its project once its run ends: its `claude` starts again there in its conversation, and is told
  where it is now as a follow-up, which carries it on (see [moving a session](worktrees.md#moving-a-session-into-one)).
- After a restart, a task comes back at rest rather than running its prompt again: its pane is drawn again
  from the transcript Claude Code keeps of its conversation (`~/.claude/projects/…/<id>.jsonl`, in
  `CLAUDE_CONFIG_DIR` when that's set), the last 512 KiB of it: the prompts, what Claude said, each tool it
  used with the first line of what came back, and how full the conversation is. What only crystal drew, the
  permissions asked for and how they were answered, and how each run ended with its cost, isn't in that file
  and doesn't come back. `crystal send` carries its conversation on. `restart-server` hands it over as it is
  instead: its `claude` goes on with the run it's in, a permission it's asking for is still there to answer,
  and its screen keeps what it showed.

`<task>` is the task's session, or its [task](#tasks) number, like `t12`.

## The backlog

Each project keeps a backlog: things worth doing later that aren't anyone's task yet. It's the project's, not
a worktree's, so every worktree of a repository shares it, and it's kept in crystal's database, out of the
repository. Items are numbered per project, `#1` on, and keep their number.

```sh
crystal backlog add "Retry the webhook on a timeout" -t payments -b "On a timeout only, not a 4xx."  # prints #4
crystal backlog                     # what's still to do; --all for what's done too, -t payments for a tag
crystal backlog show 4              # one item: its tags, its body, and the tasks started for it
crystal backlog edit 4 -b "Twice, then give up" -t payments -t ci   # its line, body or tags
crystal backlog done 4              # or reopen 4, or rm 4
crystal backlog start 4 -w          # an agent on #4, in a new worktree named after it
crystal backlog start 4 -p builder  # with a profile: its agent, options, prompt and where it starts
crystal backlog start 4 --pr 57 --background   # in the background, on a pull request's worktree
crystal backlog export > TODO.md    # markdown checkboxes, done items ticked, bodies under them
crystal backlog import TODO.md      # and back, from a file or `-` for standard input
```

- An item is a line, with a body under it when one line isn't enough (`-b`, 8 KiB at most); a line given in
  several lines is the item's line and the start of its body. A list marks an item with a body with a `+`.
- `edit` changes the line given, `-b` the body (an empty one takes it away), and `-t` the tags, in place of
  those it had (`--no-tags` takes them all away).
- `list` (or `crystal backlog` alone) takes `-t` once for each tag an item must have, `--all` and `--json`.
  `show` gives the tasks started for an item, oldest first, with how each went: an item a task failed on stays
  open, and starting it again adds another. `--json` prints it with its `tasks`.
- `import` reads every `- [ ]` or `- [x]` at the start of a line, done when it's ticked, the `#tags` at its end
  its tags (a number, like an issue's `#12`, stays in the line), and the indented lines under it its body,
  leaving out the `(#4)` an export writes. An item whose line the backlog has already is passed over, so
  importing an export into its own project adds nothing.

`backlog start` starts the agent the new-session panel picks first (`new_session` in the
[settings](configuration.md)), or a [profile](configuration.md#profiles)'s with `-p`, with the item's line and body as its task, here,
in a new worktree named after it with `-w` (or when the profile starts in one), or in a pull request's worktree
with `--pr`, told of it. `--background` runs it as a [background task](#background-tasks), with `claude -p`'s
arguments after `--`. When that task closes done, the item is ticked off. Agents are told to put what they
notice along the way on the backlog with `crystal backlog add`, rather than into the change at hand. Every
command works on the current directory's project; `-C <dir>` names another.

In the TUI, `b` opens the selected session's project's backlog, and the sidebar counts what each project has
to do beside its name: `payments ──── 3 to do`. [`/`](tui.md#finding-with-) finds an item to do on any project's
backlog, and picking it opens the backlog on it. In the view, what's to do comes first and what's done after,
and under the list, the item the bar is on: its tags, the tasks started for it and its body. `a` adds an item
and `e` changes its line, `Space` ticks one off or opens it again, `x` removes one once you've said `y`, `/`
filters the list as you type, and `t` keeps it to each tag in turn. `Enter` opens the new-session panel with
the item as its task, on a branch named after it; that task ticks the item off when it closes done.
