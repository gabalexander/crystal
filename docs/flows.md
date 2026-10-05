# Flows

<sub>[← README](../README.md#documentation)</sub>

A flow is a chain of [tasks](tasks.md#tasks) on one goal: plan it, build it in a worktree, review it, open the pull
request. Each step runs with a [profile](configuration.md#profiles) of its own, or an agent, model and effort it sets itself,
and starts once the step before it is done, given what that step answered. A step can stop the flow at a gate
until you've looked at what it did, then you go on, or send it back with notes.

- [Writing a flow](#writing-a-flow)
- [Running one](#running-one)

## Writing a flow

Flows are written in the config file, a `[[flow]]` table each, with a `[[flow.step]]` table for each step.
`crystal flow example` prints this one, with the profiles it runs with, ready to copy in:

```toml
[[flow]]
name = "ship"
description = "Plan, build in a worktree, review, open a pull request"

[[flow.step]]
name = "plan"
profile = "planner"
prompt = "Plan how to do this: {goal}. Answer with the files to change and how, in order."

[[flow.step]]
name = "implement"
profile = "builder"
placement = "fresh"
accept = ["The tests pass", "The work is committed"]
prompt = """
Do this: {goal}

Follow this plan:
{plan.summary}

{feedback}"""

[[flow.step]]
name = "review"
profile = "reviewer"
placement = "same"
gate = true
back_to = "implement"
max_rounds = 3
prompt = "Review the changes on this branch against what was asked: {goal}"

[[flow.step]]
name = "pr"
profile = "shipper"
model = "sonnet"
prompt = "Push this branch and open a pull request for it with `gh pr create --fill`."
```

| Step setting | What it does |
|---|---|
| `name` | what the step is called; its session is named after the run and it, like `ship-1-plan` |
| `profile` | optional: the [profile](configuration.md#profiles) it runs with: agent, model, mode, arguments, instructions, and its prompt and postfix around the step's; left out, Claude Code as it's set up |
| `agent` | optional: the agent it runs, like `codex`, in place of its profile's; on an agent other than its profile's, it keeps only the profile's prompt, postfix and instructions |
| `model`, `effort`, `mode` | optional: its agent's model, how hard it thinks and how it asks before acting, in place of its profile's, checked as a profile's are |
| `background` | optional: `false` runs Claude Code in a terminal rather than the background; left out, Claude Code runs in the background and any other agent in a terminal, and only Claude Code can |
| `prompt` | what it's asked, with the names below filled in |
| `accept` | optional: its [acceptance criteria](tasks.md#tasks), a list of what has to hold before it's done: its agent is told them under its prompt, and its task carries them |
| `placement` | optional: where it runs, below; left out, where the step before it ran, and the first where the run started |
| `worktree` | optional: `true` is `placement = "fresh"`, as it was first written |
| `gate` | optional: `true` stops the flow after it until you go on, or send the flow back |
| `back_to` | optional, on a step with a gate: the step that sending the flow back runs again; left out, this one |
| `max_rounds` | optional, on a step with a gate: how many rounds the flow may take through it, 1 to 3, left out 3; in the last, it can't be sent back |

| Placement | The step runs |
|---|---|
| `root` | where the run was started |
| `fresh` | in a worktree the run makes for itself, the first time a step asks for it, on a new branch with a made-up name like `brave-otter`, as the new-session panel gives a new worktree (`brave-otter-2` when that's taken); every `fresh` step of the run after that runs there too |
| `same` | where the step before it ran |

A step in a worktree is told about the [handoff file](tasks.md#the-handoff-file) there like any agent, so it hears what
the steps before it there noted, and how each of their tasks ended.

What a prompt can name, in braces:

| Name | What it's filled in with |
|---|---|
| `{goal}` | what you asked the flow to do |
| `{slug}` | the goal's first line as a branch would have it: `add-retries` |
| `{round}` | 1, and one more each time the flow is sent back |
| `{previous}` | what the step before this one answered |
| `{<step>.summary}` | what the step called `<step>`, before this one, answered last |
| `{<step>.artifacts}` | the paths of the files that step's task [kept](tasks.md#kept-files), one after another |
| `{feedback}` | empty until you send the flow back; from then on your notes, and when it went back to an earlier step, what the step at the gate said |

The step the flow goes back to hears the feedback even if its prompt doesn't ask for it. A brace that names
none of these stays as it is, so a prompt can show code; a `{<step>.summary}` or `{<step>.artifacts}` naming no
step before it is an error. A prompt is kept to 16 KiB: past that, what steps answered is cut short, the oldest
first, each ending `[cut short]`, and a step whose own text is too long fails.

## Running one

```sh
crystal flow run ship "Retry the webhook when it times out"    # prints the run's name: ship-1
crystal flow                         # every run: how it stands, its step, round and cost
crystal flow show ship-1             # each step: how it stands, its session, runs, cost and answer
crystal flow wait ship-1             # until it waits at a gate or is done; a failed step is an error
crystal flow approve ship-1          # go on past the gate
crystal flow back ship-1 "Keep the old timeout as the default"    # send it back, with notes
crystal flow retry ship-1            # run a step that failed, or that a restart cut short, again
crystal flow cancel ship-1           # cancel its step's task, and go no further
crystal flow defs                    # the flows a run started here finds, and where each is written
```

- A step on Claude Code runs as a [background task](tasks.md#background-tasks), so nobody is there to say yes to a
  permission: give it a profile that allows what it needs, with `mode` and `args`. A step on any other agent,
  like Codex, or on Claude Code with `background = false`, runs in a terminal, a session with the step as its
  [task](tasks.md#tasks), where you can watch it and answer it, and the flow goes on once that task closes: done with
  its summary as the step's answer, or failed. An agent that can't be given a prompt to start on, like Aider,
  can't be a step. Each step's task goes into the project's [history](tasks.md#tasks) as `ship-1 plan: <goal>`.
- Sending the flow back runs the step it goes back to again, in a new round: a background step as a follow-up
  in its own conversation, a step in a terminal in a new session, with the old one left for you to read. Then
  the steps after it run again. In a gate's last round, `max_rounds`, it can't be sent back: approve it, or
  cancel the run. A step that fails stops the run until you run it again, and you're told, as you are when a run stops at
  a gate.
- `crystal flow cancel` cancels the task of the step the run is at, while it's open, stops its session, and
  the run goes no further: it's `cancelled`.
- A project can keep flows of its own in `.crystal/flows.toml` in its main worktree, `[[flow]]` tables like the
  config file's, run with the profiles in your config. They're found by any run started in the project, and
  one there takes the place of the config file's of the same name. `crystal flow defs` lists the flows a run
  started in the current directory (or `-C <dir>`) finds, each with its steps, or why it can't run, and the
  file it's written in.
- In the sidebar, a run sits under its project after its worktrees: `◇`, the flow's name and the goal, and the
  round once it's been sent back. Under it is a row for each step: `·` still to come, the working mark while it
  runs, `▲` at its gate, `✓` done, `✗` failed, `■` cut short and `–` cancelled. A step's row is its task's
  session, so selecting it shows the step's transcript.
- At a gate, the step's session waits on you the way an agent asking something does: you're told, `u` goes to
  it, and `ls` says `waiting`. `g` goes on, and `f` asks for your notes on the footer and sends the flow back,
  on the step's row or in the list `U` opens. On a step that failed or was cut short, `g` runs it again.
- The new-session panel offers the flows in your config file after your profiles, `flow: ship`; what you type
  as the task is the goal.
- Runs are kept with the sessions, in crystal's database. After a restart, a run waiting at a gate waits
  again, a step in a terminal whose session comes back carries on there, and a background step that was
  running is marked interrupted until you run it again, in its conversation; `restart-server` hands runs over
  as they are, every step running carrying on. A run's steps start from the environment of the `crystal flow
  run` that started it; after a restart, from the daemon's. The daemon reads the flow and its profiles as the
  run starts, so changing them never changes a run halfway.
