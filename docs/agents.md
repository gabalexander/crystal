# Agents

<sub>[← README](../README.md#documentation)</sub>

How crystal knows what an agent is doing, through its hooks and by reading its screen, how to give an agent
you start yourself crystal's hooks, what's particular to Codex, and how an agent crystal doesn't know can tell
it what it's doing.

- [What an agent is doing](#what-an-agent-is-doing)
- [Agents you start yourself](#agents-you-start-yourself)
- [Codex](#codex)
- [How crystal reads an agent](#how-crystal-reads-an-agent)
  - [Hooks in other agents' own settings](#hooks-in-other-agents-own-settings)
- [Teaching crystal about your agent](#teaching-crystal-about-your-agent)
  - [What it's doing](#what-its-doing)
  - [How to resume it](#how-to-resume-it)
  - [Letting go](#letting-go)
  - [A line on its row](#a-line-on-its-row)
  - [Tokens, a title and labels for a layout](#tokens-a-title-and-labels-for-a-layout)
  - [Numbered reports](#numbered-reports)
  - [Keep it out of the way](#keep-it-out-of-the-way)

## What an agent is doing

crystal knows what an agent is doing in two ways. When it starts Claude Code itself, it adds hooks with
`--settings`, so your settings files are left alone and your own hooks still run. And for every session it
reads the screen, by [rules for each agent](#how-crystal-reads-an-agent): the spinner an agent puts in its
title, "esc to interrupt" while it works, the question it asks before a command. The screen covers agents
without hooks, like Codex or a Claude you started from a shell, and what hooks never say: a turn you cut short
with Esc, or work carrying on once you've said yes. The screen only counts while an agent is in front: a shell
or a build printing an agent's words never shows as waiting, and when an agent exits back to its shell, what
it was doing goes with it. `crystal agent explain <session>` shows why crystal reads a session the way it
does. While Claude Code's agent has subagents running, its row says how many after
what's in front: `claude +2`.

Claude Code runs subagents in the background: its turn ends and its prompt comes back while they go on, and it
takes up what each finds in a turn of its own. So a turn that ends with subagents running isn't the agent done:
it reads as working, isn't `✓`, tells you nothing, and an open task isn't waiting on you nor reminded of itself,
until they've all stopped and the agent has taken their work up. crystal looks four times a second: once the last
one has stopped, a turn of the agent's own, its prompt or its spinner, is the work going on, and its end the
end of it; with none within a minute, the turn is over. Subagents that show no sign of life for 15 minutes,
none starting or stopping, no tool finishing, no permission asked for, are taken for gone, and the turn is over
too.

So with the rest of the work Claude Code wakes the agent for, as its Stop hook lists it (`background_tasks` and
`session_crons`): a command it runs in the background, like a test suite, a Monitor watching CI, a wakeup it
scheduled (`ScheduleWakeup`, `CronCreate`, `/loop`), or another task Claude Code keeps for it. A turn that ends
with any of it to come reads as working, its row says on what (`in the background: cargo test`, `watching: CI
checks`), and it tells you nothing, until a turn of the agent's own starts as that work wakes it; that turn's
end is held again if some is still to come. If the agent never wakes, the turn is over once the longest the work
can take has passed, and a minute more: two hours for a command, an hour for a Monitor or a wakeup. A turn whose
last message asks you something is never held: you're told at once, whatever runs.

## Agents you start yourself

A Claude Code or Codex you start yourself, typed into a session's shell, has no hooks of crystal's: crystal
doesn't start it. `crystal integration install` puts crystal's hooks in their own settings, beside yours:

```sh
crystal integration install          # each agent crystal can hook that's installed here
crystal integration install claude   # into $CLAUDE_CONFIG_DIR/settings.json, or ~/.claude/settings.json
crystal integration install codex    # into $CODEX_HOME/hooks.json, or ~/.codex/hooks.json
crystal integration install kimi     # and 14 more agents' hooks or plugins: see below
crystal integration status           # whether they're there, for this crystal
crystal integration status --outdated-only   # only those out of date: install brings them up to date
crystal integration uninstall        # take them out again, and only them
```

`status` says `installed` when each of crystal's hooks is there as this crystal would put it, `out of date`
when some are there but not so (another crystal's, at another path, or an earlier one's, before crystal
listened to an event it does now), and `not installed`. The [settings view](configuration.md#the-settings-view) lists the
agents installed here too, and puts their hooks in, brings them up to date or takes them out with `space`.

Then the agent you typed says what it's doing through its hooks, the same as one crystal starts, and which
conversation it's in: after a restart, the session's shell starts again and `claude --resume <id>` (or `codex
resume <id>`, or [the other agents' own](#hooks-in-other-agents-own-settings)) is typed into it, so you're
back where you were. Only while the agent is in front, though: quit it, and the shell comes back on its own.
`resume_reported_agents = false` in the [settings](configuration.md) turns that off. Each hook runs `crystal hook <agent> --installed`, crystal by its path, so run `install` again if
you move crystal; `status` says when the hooks are out of date. Outside crystal, and for an agent crystal
started with hooks of its own, they do nothing.

## Codex

crystal reads what Codex is doing off its screen: `Working (… esc to interrupt)` while it works, and its
approval questions ("Would you like to run the following command?") while it waits on you. Codex has hooks
too, but crystal can't add its own as it starts Codex, the way it does for Claude Code: Codex only reads hooks
from its config files, never from the command line, and skips any hook you haven't reviewed. Its `notify`
setting can be given on the command line, but that would replace yours, so crystal leaves it alone.

`crystal integration install codex` puts crystal's hooks in Codex's `hooks.json` instead, and turns on
`[features] hooks` in its `config.toml`, keeping the rest as it was. Codex asks you to review new hooks as it
starts, or in `/hooks`; once you've trusted them, every Codex reports through them, the ones crystal starts and
the ones you type into a shell: a turn starting and ending, the permissions it asks for, its subagents and its
conversation. A turn you cut short with Esc ends it too. Codex runs hooks in the background server its
sessions share, so they can't tell crystal which session they're in: crystal goes by the conversation they
name, and for a conversation it doesn't know yet, by the session Codex was just started in. Codex sessions
started at the same moment may not be told apart until each has been sent a prompt; `codex --no-daemon` keeps
a Codex's hooks to itself.

To pick a conversation up again, crystal finds the file Codex records it in,
`$CODEX_HOME/sessions/YYYY/MM/DD/rollout-…jsonl` (`~/.codex` without `CODEX_HOME`): the one for the session's
directory that Codex started closest to when the session did, within a minute, unless its hooks have named
it. After a restart, or with `crystal respawn`, the session runs `codex resume <id>` with the options it was
started with, but not its first prompt again. The limits:

- Codex writes that file once it has been sent a prompt, so a Codex that was never sent one starts afresh.
- A conversation you begin from inside Codex with `/new` isn't followed, unless its hooks are installed:
  crystal picks the first one up again.
- `codex exec` and Codex's other subcommands run as they were asked, without resuming.
- A Codex you start yourself in a shell session gets its status from the screen, and is resumed only with
  crystal's hooks installed.

## How crystal reads an agent

An agent is known by what's in front in its session's terminal: the program, as it was run, or the script an
interpreter like `node` runs. A wrapper that hides the agent's own process, like a sandbox, says which agent it
runs with `CRYSTAL_AGENT` in its environment: `CRYSTAL_AGENT=claude fence -- claude`. Only the process in front
is looked at, so set it on the wrapper's command rather than exporting it, which would make every program you
run that agent. A Mac keeps the environment of the programs it ships, like `env` or `sh`, to itself, so there it
works for a wrapper you installed.

crystal reads each agent's screen by a file of rules for that agent. It comes with one for each of Claude
Code, Codex, Gemini CLI, OpenCode, Cursor, Qwen Code, Pi, GitHub Copilot, Amp, Droid, Kimi Code, Kiro, Cline,
Kilo Code, Devin, Grok, Qoder CLI, Letta Code, Hermes Agent, Antigravity, Maki and Muse, adapted from
[herdr](https://github.com/herdrdev/herdr)'s, and a common one for any other agent in front, like Aider.
MastraCode's file only names it: it says what it's doing through [its hooks](#hooks-in-other-agents-own-settings)
alone. An
agent changes what it draws from one version to the next, so when crystal reads one wrong you can mend its
rules yourself without waiting for a release:

```sh
crystal agent list                        # the agents, where their rules come from, installed, hooks
crystal agent explain fix-login           # why crystal reads that session the way it does
crystal agent explain fix-login --agent codex -v   # Codex's rules on its screen, with what each looked at
crystal agent explain --file screen.txt --agent codex --title "⠋ codex"   # rules on a saved screen
crystal agent rules codex > ~/.config/crystal/agents/codex.toml           # start from crystal's own
```

A file in `~/.config/crystal/agents/` takes the place of crystal's rules for the agent its `id` (or one of its
`aliases`) names, and a file for an agent crystal has none for adds it: it's then taken for an agent when it's
in front, and read by its rules. The daemon reads the files again within a couple of seconds of a change. A
file that can't be used is said in `crystal agent list`, `explain` and the daemon's log, and crystal's own
rules stand in for it, so a typo never stops the reading.

```toml
# A file's fields, and every test a rule can make.
id = "codex"                     # the agent, by its program's name
name = "Codex"                   # how crystal shows it
aliases = ["codex-cli"]          # other names its program goes by
packages = ["@openai/codex"]     # npm packages it runs from, as `node …/node_modules/<package>/…`

[[rules]]
id = "approval_question"         # what explain calls it
looks = "waiting"                # working, waiting, settled, or skip
priority = 890                   # of the rules that match, the highest wins; the first in the file on a tie
region = "last_rows(15)"         # where it looks
contains = ["would you like to"] # all of these, in any case
regex = ['\(y\)']                # all of these patterns match
line_regex = ['^› ']             # each of these matches a line
any = [{ contains = ["yes"] }, { contains = ["❯"] }]   # one of these passes
all = [{ contains = ["proceed"] }]                     # every one of these passes
not = [{ contains = ["esc to interrupt"] }]            # none of these passes
```

`skip` is for a screen that says nothing either way, like a menu or a transcript viewer over the prompt: the
status stays as it was. When no rule matches, the agent is settled. A rule looks in one region:

| Region | What it is |
|---|---|
| `screen` | the whole screen (the default) |
| `title` | the title the agent gave its terminal |
| `progress` | the progress it reports (OSC 9;4), as `4;1;-1`: a state, then a percentage |
| `last_rows(N)`, `first_rows(N)` | the last or first N rows with something on them |
| `after_last_rule` | the rows after the last horizontal rule (`───`) |
| `prompt_box`, `above_prompt_box`, `last_row_above_prompt_box` | inside the box between the last two rules, what's above it, and its last row |
| `after_last_prompt`, `before_current_prompt`, `without_current_prompt` | around Codex's prompt line, `›`: the rows after the last one, the rows before the one the user is at, or the whole screen unless the user is at one |

A new look counts once two checks in a row see it, so a screen caught halfway through a redraw doesn't.

### Hooks in other agents' own settings

Beyond Claude Code and Codex, some agents take hooks only in their own settings files, never on the command
line, and some take plugins instead. crystal leaves those files alone unless you ask, with the same [`crystal
integration`](#agents-you-start-yourself) command, as for those two:

```sh
crystal integration install cursor     # ~/.cursor/hooks.json, or $CURSOR_CONFIG_DIR's
crystal integration uninstall cursor   # takes crystal's out, and leaves yours
```

| Agent | `crystal integration install …` | Where | What it says | Resumed with |
|---|---|---|---|---|
| Cursor | `cursor` | `~/.cursor/hooks.json` (`$CURSOR_CONFIG_DIR`) | a turn ending, its conversation | `cursor-agent --resume <id>` |
| Droid | `droid` | `~/.factory/settings.json` | its turns, its session | `droid --resume <id>` |
| Qoder CLI | `qodercli` | `~/.qoder/settings.json` (`$QODER_CONFIG_DIR`) | its turns, what it asks, its session | `qodercli --resume <id>` |
| Qwen Code | `qwen` | `~/.qwen/settings.json` (`$QWEN_HOME`) | its session | `qwen --resume <id>` |
| GitHub Copilot | `copilot` | `~/.copilot/settings.json` (`$COPILOT_HOME`) | its session | `copilot --resume=<id>` |
| Devin | `devin` | `~/.config/devin/config.json` (`$XDG_CONFIG_HOME`) | its turns, its session | `devin --resume <id>` |
| Kimi Code | `kimi` | `[[hooks]]` in `~/.kimi-code/config.toml` (`$KIMI_CODE_HOME`) | its turns, what it asks, its session | `kimi --session <id>` |
| Letta Code | `letta` | `~/.letta/settings.json` | its conversation | `letta --conversation <id>` |
| MastraCode | `mastracode` | `~/.mastracode/hooks.json` | its turns, what it asks, its thread | `mastracode --thread <id>` |
| Grok | `grok` | `~/.grok/hooks/crystal.json` (`$GROK_HOME`) | its session | `grok --resume <id>` |
| Antigravity | `agy` | a `crystal` block in `~/.gemini/config/hooks.json` (`$ANTIGRAVITY_CLI_CONFIG_DIR`) | its conversation | `agy --conversation <id>` |
| Pi | `pi` | an extension, `~/.pi/agent/extensions/crystal.ts` (`$PI_CODING_AGENT_DIR`) | its turns, its session file | `pi --session <file>` |
| OpenCode | `opencode` | a plugin, `~/.config/opencode/plugins/crystal.js` | its turns, what it asks, its session | `opencode --session <id>` |
| Kilo Code | `kilo` | a plugin, `~/.config/kilo/plugin/crystal.js` | its turns, what it asks, its session | `kilo --session <id>` |
| Hermes Agent | `hermes` | a plugin in `~/.hermes/plugins/crystal/`, switched on in its `config.yaml` (`$HERMES_HOME`) | its session | `hermes --resume <id>` |

Each gets the events it has that say what it's doing or which conversation it's in, as far as they can be
trusted, as [herdr](https://github.com/herdrdev/herdr) installs its own: where an agent's hooks miss a turn
cut short or a permission it cancels, crystal takes only those that name its conversation, and reads the rest
off its screen, by its [rules](#how-crystal-reads-an-agent). MastraCode has no rules for its screen, so it
shows a status only with its hooks in. A hook runs `crystal hook <agent>` inside a crystal session only, so the
agent anywhere else runs as before, and it never fails the agent; a plugin runs it the same way. What a hook
says counts only while that agent is in front: an agent that Claude Code runs in the session doesn't speak for
the session. `crystal agent list` says whose hooks are in. A JSON settings file is written again as formatted
JSON, its keys in order; Kimi's TOML and Hermes's YAML keep the rest as it was. A plugin is loaded as its
agent starts, so start again one that's running; `status` says when a plugin is out of date.

Once its hooks have named the conversation, an agent crystal started comes back in it after a restart, with
`crystal respawn`, or from the archive, with the options it was started with but not its first prompt again,
the way Claude Code and Codex do; one typed into a shell has that command typed in again. A conversation only
counts once the agent has worked on a turn in it (or written the file its hooks say it keeps it in): before
that there's nothing to pick up, and the agent starts afresh. The limits:

- OpenCode's plugin runs in its server: an OpenCode attached to a server it shares with other sessions
  (`opencode attach`) reports to the session that started the server, if any.
- Letta's default conversation is resumed with its agent, `letta --conversation default --agent <agent>`.
- An option of the agent's own that chose a conversation, like `--continue`, gives way to crystal's choice.

## Teaching crystal about your agent

crystal knows Claude Code by its hooks, and reads Codex and the other agents it has
[rules](#how-crystal-reads-an-agent) for off their screens. Any other agent, or a script wrapped around one,
can tell crystal what it's doing itself, and how to pick its session up again, with `crystal report`: no
change to crystal, and no waiting for a release of it. Once your
agent reports, its status shows in the sidebar and in `crystal ls`, the user is told when it's done with a turn
or waits on them, `crystal wait` and the [events](events.md) follow it, and, once it says how, its session comes
back in the same conversation after crystal restarts.

Every program in a session has these in its environment:

| Variable | What it is |
|---|---|
| `CRYSTAL_SESSION` | the session's name when the program started |
| `CRYSTAL_SESSION_ID` | the session's id, which a rename never changes |
| `CRYSTAL_SOCKET` | the daemon's socket, which `crystal` finds it by |
| `CRYSTAL_SERVER` | the daemon's [server](servers.md#servers), when it isn't the default one |

Report only when `CRYSTAL_SESSION_ID` is set. Outside crystal there's no one to tell, and `crystal report`
fails, saying so.

### What it's doing

```sh
crystal report working --agent my-agent         # a turn has started
crystal report waiting -m "approve the deploy"   # it needs the user to decide; blocked says the same
crystal report idle                              # at its prompt, ready for the next
crystal report done                              # it finished a turn
```

Report `working` as a turn starts, `idle` when your agent is ready for input, and `waiting` when it needs the
user; `-m` says what for, in the notification and the event log. `idle` after `working` ends a turn, the same
as `done`: the session shows `done` until someone looks at it, and the user is told. `--agent` is the name the
sidebar and `ls` show for it, one word; without it, it's the name given before, or what's in front in the
session. A report is about the session it's run in; `-n <session>` names another.

The first report takes the session over: from then on, its reports are the session's status, and crystal
reads neither its screen nor Claude Code's hooks for it, until your agent lets go.

### How to resume it

Put the command that picks the current session up again after `--`, with the options that session needs, so
it comes back the same:

```sh
crystal report idle -- my-agent --resume "$SESSION_ID" --model my-model
crystal report --session-only -- my-agent --resume "$NEW_ID"     # only the command, when the session changes
```

After crystal restarts, from a crash, a reboot or `crystal restart-server --cold`, the session starts again in
its directory and runs that command: typed into the session's shell when the session runs one, the way you
started your agent, or else in place of the session's own command, which `ls` still shows. Then your agent
says what it's doing again, as it did the first time, command and all. `crystal respawn` does the same for a
session that ended while your agent held it. `crystal restart-server` hands the session over instead: your
agent goes on running, and still holds it. A command that breaks these rules is refused, and the report
with it:

- Its first word is a plain command name found on the `PATH`, like `my-agent`, not a path.
- No word holds a quote (`'`) or a control character, so every shell reads it the same.
- At most 64 words, and 8 KiB in all.

`--session-only` needs your agent to hold the session already: report what it's doing first, or along with
the command. `resume_reported_agents = false` in the [settings](configuration.md) starts sessions again with their
own commands instead.

### Letting go

```sh
crystal report --release
```

When your agent quits, it lets go: crystal reads the session for itself again, and forgets the agent's name
and command. Let go only when the user quits; an agent that swaps one session for another reports the new
one instead. An agent that leaves without letting go is let go of once the shell is back in front, a moment
later: a safety net, not a way to leave. That's also why a report typed at the shell's own prompt doesn't
hold: report from your agent's process.

### A line on its row

```sh
crystal report --line "indexing 40%"                    # a line under the session's row in the sidebar
crystal report --line "deploying" --ttl 5m              # gone in five minutes unless it's reported again
crystal report --model my-model-large                   # the model it runs on, beside its name
crystal report --source indexer --seq 12 --line "60%"   # one numbered lower than the last from indexer is dropped
crystal report --line ""                                # take it off
```

`--line` and `--model` are for the sidebar alone: on their own, they don't take the session over, so an agent
crystal already knows, or a script running beside it, can say what it's on without changing how its status is
read. With a state, `crystal report working --line "reading the docs"`, they go along with it. Each stays until
it's said again or taken off with `""`, or for as long as `--ttl` gives it (`30s`, `5m` or `2h`, a day at most);
`--model ""` gives back the model crystal reads. Text is put on one line, without control characters, and cut to
80 characters.

### Tokens, a title and labels for a layout

```sh
crystal report --token load=93 --token ci=green         # values for a layout's $load and $ci
crystal report --token ci=                              # take one off; the others stay
crystal report --title "refund fix"                    # what `title` says in place of the session's name
crystal report --display-agent pi                       # the agent its row says is in front, as `agent` does too
crystal report --state-label waiting="needs a key"      # what `state` says while it waits; again for another
crystal project report --token deploy=green --ttl 10m   # a value for the project's heading and worktrees, $deploy
```

These are values for rows [laid out your own way](tui.md#laying-out-its-rows), and like `--line`, for the sidebar
alone: they don't take the session over, go with `--ttl`, `--source` and `--seq` the same way, and are cut to 80
characters. A token's name is letters, digits, `_` and `-`, starting with a letter, and a session or a project
shows 32 at most. `--display-agent` shows on crystal's own row too, where it says what's in front.
`crystal project report` puts its tokens on the project the directory it's run in (or `-C`) is in, for its
heading's and its worktrees' `$name`.

### Numbered reports

```sh
crystal report working --source my-agent --seq 41      # one numbered no higher than 41 from my-agent is dropped
crystal report --release --source my-agent             # lets go only of a session my-agent holds
```

Any report can name who sends it, with `--source` (letters, digits and `:._-`), and number it with `--seq`,
which goes up with every report from that source, across your agent's restarts too: a timestamp works. A
report that arrives after a later one from the same source is passed over, so a hook that runs late, or
reports sent at once from several processes, can't put back what your agent was doing before, or leave an
older line showing. What it's doing and what's on its row are numbered apart, so one command can say both
under one number. The source whose report took the session over is the one that lets go of it: a
`--release` from another source is passed over, and one with no `--source` always lets go. A session takes
numbered reports from up to 32 sources.

### Keep it out of the way

- Don't let crystal hold your agent up: report with a short timeout, one report at a time, and ignore
  failures.
- `crystal ls --json` shows what crystal has: `reporter`, with the agent's name, its last `message`, the
  `resume` command and the `source` that holds the session, `front`, the agent by its name, and the `line` and `model` its row shows.
- `crystal events -n <session>` shows each report that changed something, and `session.claimed` and
  `session.released` as your agent takes the session over and lets go.
