# Sessions

<sub>[← README](../README.md#documentation)</sub>

Starting a session, what happens to it when crystal restarts, archiving it or letting it stop when it sits
idle, how crystal tells you it needs you, and the memory and CPU each one takes.

- [Starting a session](#starting-a-session)
- [After a restart](#after-a-restart)
- [Where it's kept](#where-its-kept)
- [Archiving and idle agents](#archiving-and-idle-agents)
- [Notifications](#notifications)
- [Sounds and the bell](#sounds-and-the-bell)
- [Resources](#resources)

## Starting a session

`n` opens the new-session panel over the panes, titled with where the session will start: `New session ·
payments ⌂ main`. Type what the agent should do and press `Enter`; the task is its first prompt, given as one
argument. `Alt+Enter` starts a new line, and a paste keeps its lines. An empty task starts the agent with no
prompt. `↑` on the first line and `↓` on the last bring back earlier tasks: the panel keeps the last 100, in
crystal's database. The task box edits [as a shell's line does](keys.md#text-boxes), `Ctrl+W` and `Alt+B` among
its keys, all but `Ctrl+E`, which is the panel's.

Drag a file onto the task, or onto the reply box, and your terminal pastes its path. A screenshot dragged from
macOS's floating thumbnail is a file macOS deletes soon after the drop, long before the agent gets to read it,
and its name has a narrow no-break space before `PM` that an agent types back as a plain space. So crystal
copies a dropped file from a folder that goes away (`TemporaryItems`, or on a Mac the temporary directory macOS
clears), and an image whose path has anything a shell would want escaped, into its state directory
(`~/.local/state/crystal/attachments/`) under a plain name, like `Screenshot-2026-09-21-at-11.13.58-PM.png`,
and puts the copy's path in the box in place of the one dropped. Any other file, like a source file in the
worktree, is left as pasted, for the agent to work on the file itself. A copy is kept for a week from when it
was last dropped: older ones are deleted as another is copied, and as the TUI starts.

`Esc` puts the panel away without losing what's in it: the next `n` or `w` opens on it, `draft left last time`
under it. Opened where it was before, it comes back whole, the task, what runs it, its rows' choices, where it
starts and the branch; opened somewhere else, `n` on another session or `w` after `n`, the task and the rows'
choices come back, and it starts where it's opened now. The draft lasts while the TUI runs, and goes once a
session has started from it; a session that couldn't start leaves it for the next `n`. Empty the task and put
the panel away to drop it. A panel opened for an issue, a pull request or a backlog item brings its own task,
and leaves the draft as it was.

The session is named for its task: its first few words that say what it's about, like `fix-refund-rounding`
for "Fix the refund rounding, please", with `-2`, `-3` added if that's taken. Started with no task, it's named
after its program, `claude`, until its first prompt names it: Claude Code tells crystal each prompt it's sent,
and the first that has words to go on names it, a slash command never. A name you give, or rename a session
to, stays, and so does one a script has typed into the session by with `crystal send` or `send-keys`.
`crystal new` and `crystal task` name a session the same way when you don't, and print the name.
`name_from_prompt = false` in the [settings](configuration.md) names sessions after their programs.

A prompt's first words don't always say what it's about, so Claude Code names a session better: with the first
prompt you send it that says what you're asking for, crystal asks it to name the session in three or four words
of its own, which it does with `crystal name`, like `crystal name Fix Login Redirect`, and the session is called
`fix-login-redirect` from then on, as docket's AUTO-TITLE does. It's asked once, with that prompt alone, as
Claude Code's hook tells crystal of it, and allowed the command without asking you. Only a session you start
from the TUI, or with `crystal new` attaching it in your terminal, is named this way: one whose name crystal
printed, with `-d` or for a script, keeps it, since whoever started it may know it by it, and so does one you've
named or renamed, or a script has typed into by its name. A rename in Claude Code is followed all the same.
`name_by_agent = false` in the [settings](configuration.md) keeps the name from the prompt.

Claude Code's own name for a conversation, the one `/rename` gives it, and the session's name in crystal are
kept in step, as far as Claude Code lets them be. `/rename Fix refund rounding` renames the session
`fix-refund-rounding` within a moment, as crystal reads the name Claude Code keeps beside its transcript,
unless you named the session yourself: a name you gave it, with `r`, `crystal rename` or `-n`, stays, since
scripts and other agents may know it by it. The other way, a name you rename a session to in crystal becomes
Claude Code's for the conversation, shown in its prompt box and `/resume`, but only with the next prompt you
send it: nothing renames a Claude Code session from outside but its hook's answer to a prompt. The name a
session started with isn't given to Claude Code, nor one crystal made up: Claude Code has its own. Only Claude Code has such a name to keep in step; a
background task has none.

`D` starts a session like the selected one: it opens the panel set to what that session runs, the profile its
command fits or else its agent, each row at the option its command gives, in the background for a background
task, and in its place, so only the task is left to write. Anything on its command line the panel has no row
for, like `--dangerously-skip-permissions`, comes along, there to see in the command the panel shows. Its
conversation doesn't: the new session starts fresh. On a program that's no agent crystal knows, `D` hands its
command to the bottom line, `new session:`, to run again or change first.

Under the task, `Tab` and `Shift+Tab` go from row to row and `←` / `→` change a row's choice:

- **run**: your [profiles](configuration.md#profiles), then the agents installed on your `PATH` (Claude Code, Codex, Gemini
  CLI, OpenCode, Cursor, Qwen Code, Pi, GitHub Copilot, Amp, Droid, Kimi Code, Kiro, Cline, Kilo Code,
  Devin, Grok, Qoder CLI, Letta Code, Hermes Agent, Antigravity, MastraCode, Aider), then your shell. What you started
  last is chosen the next time. A profile's
  description shows under the row, and choosing it sets the rows below from it; you can still change them.
- **how**, for Claude Code: **in a terminal**, or **in the background**, as a [background
  task](tasks.md#background-tasks) that needs no terminal. Only Claude Code offers it: crystal reads `claude -p`'s
  events for a task's transcript, and Codex's `codex exec` writes another kind it doesn't read yet.
- Claude Code's **model** (fable, opus, sonnet, haiku), **effort** (low to max) and **permissions** (`--model`,
  `--effort`, `--permission-mode`), or Codex's **model** and **approvals** (`-m`, `-a`), its models the ones
  `codex debug models` lists. Left at `default`, no option is added.
- **start in**: here (the selected session's worktree, or where you started `crystal`), a new worktree, or
  the main worktree of another of the [projects](worktrees.md#projects) crystal knows, sessions running there or not.

A new worktree's **branch** gets a made-up name, an adjective and an animal like `brave-otter`, whatever the
task says; type in that row to change it. A made-up name is always a new branch: if it's taken, the worktree
goes on `brave-otter-2`. One you type that's a branch already is checked out as it is. `w` opens the panel
with a new worktree chosen, and `Enter` on an issue opens it ready to fix that issue, on a branch named after
it.

The panel ends with the command it runs and, for a new worktree, where. `Ctrl+E` hands that command to the
bottom line, `new session:`, to change it or run anything else: `npm run dev` or `sh -c 'make && make test'`
work there, and an empty line starts your shell. Claude Code, Codex, Gemini CLI, OpenCode, Cursor, Qwen Code
and Pi are given the task as they start, each the way it takes one. For the others crystal knows of no way to,
checked against their own code or documentation, so for them the task box gives way to a note.

## After a restart

If the daemon dies without being asked to, because it crashed or the machine rebooted, the next `crystal` starts
the sessions that were running again, in the same directories and in their places in the list. Claude Code and
Codex come back in the conversation they were in. Shells and other programs start straight away, and so does
the first agent, but the agents after it start a quarter of a second apart (`restart_spacing_ms` under
`[sessions]`, `0` for all at once), so a dozen of them don't all load at once; until its turn, an agent's row
says `starting`. A session that can't start again, because its directory has gone or its command isn't there
any more, isn't dropped, and never starts somewhere else instead: it stays in its place, its row says
`couldn't start`, its screen and `crystal ls` say why, and it stays written down, to try again with the next
restart. It's pinned with what [needs you](tui.md#the-sidebar), and `U` lists it with why. Put it right and `Enter`
on it (or `crystal respawn`, or `r` in `U`'s list) starts it, or kill it. Once they've all started
or failed, the TUI's footer says how it went, like `after the restart: 6 sessions back · 1 couldn't start:
docs`, and the [event log](events.md) has a `session.start_failed` for each that couldn't and a
`daemon.restarted` for the lot. `crystal kill-server` is asked to stop everything, so after it nothing comes
back. A session comes back under its name, and one you or a script named keeps it through a `/rename` in
Claude Code, as [before the restart](#starting-a-session).

A shell or any other program starts again afresh, on a clear screen. With `restore_screens = true` under
`[sessions]`, its terminal shows what it showed before instead, history and all, above a line saying crystal
restarted, and its program starts under it: crystal keeps what each terminal shows in its database, as soon as
there's something on it and then at most every 15 seconds while it changes. It's off unless you turn it on,
since a screen can show secrets, like a token a command printed. Claude Code and Codex picked up in their
conversation show their own, so their screens aren't kept, and a background task draws its transcript again.
Turning it off forgets what was kept, and so does `crystal kill-server`.

## Where it's kept

The list is kept in crystal's database, `~/.local/state/crystal/crystal.db`, without the sessions' environment
variables, since those can hold secrets; a session started again gets the environment of whoever started the
daemon again.

The database is SQLite, and holds everything crystal keeps but its settings and memory: the sessions to start
again, flow runs, each project's backlog and closed tasks, and the TUI's tabs, layouts, earlier tasks and the
files you've marked reviewed in the diff. Each
change is written whole or not at all, so a crash or a power cut never leaves half of one, and a database that
can't be read stops the daemon rather than being started over. What an older crystal kept in JSON files there is
brought in the first time, and each file is kept beside it, renamed `.imported` (or `.broken`, when it couldn't
be read).

## Archiving and idle agents

`A` archives the selected session, once you've said `y`: it stops, as `x` would, and leaves the list, but
crystal keeps what it takes to start it again, in the archive. `Z` opens the archive, the latest archived
first, each with where it ran and how long ago: `Enter` starts the one the bar is on again, under its name (or
the next one free, if that's been taken since), and `x` deletes it for good once you've said `y`. Claude Code
and Codex come back in the conversation they were in, as after a restart, and so does an agent whose
[hooks](agents.md#hooks-in-other-agents-own-settings) named its conversation, or that [said how to resume
it](agents.md#teaching-crystal-about-your-agent); anything else starts its command again from the
top, and the archive says which. An archived session's open task is cancelled, and open again when it comes
back. From the command line, `crystal archive <name>`, `crystal unarchive <name>` and `crystal ls --archived`
do the same, and `crystal kill` on an archived name deletes it.

An agent you've left alone is stopped for you, to free what it holds: an idle Claude Code takes a few hundred
MB with its MCP servers. Once an agent has sat at its prompt for half an hour (`stop_idle_after` under
`[sessions]` in the [settings](configuration.md), like `"2h"`, or `"off"`), its turn seen, with nobody watching it or
typing into it, crystal stops it. It stays in the list, its row saying `stopped idle`, and comes back in its
conversation as soon as you go to it: once the sidebar's selection rests on it a moment, or `crystal attach`
reaches it, as well as on `Enter` or `crystal respawn`. A message `crystal send` gives it, another agent's or
the reply box's, starts it again too, and reaches it once it's back at its prompt. A crash or a reboot leaves
it stopped, to come back the same way. Only an agent that can come back where it was is stopped: Claude Code,
Codex or another agent once crystal knows its conversation, or an agent that said how to resume it. A turn that
ended while you were away waits for you (`✓`) however long it takes, and so does one asking you something;
background tasks and sessions with their task open are never stopped. Nor is an agent with work going on
without it: subagents running, a job it cut loose from its terminal still running, like Claude Code's Bash
calls and Monitor watches in the background or a Codex shell command, whose end starts its half hour again, or
a wakeup it scheduled, Claude Code's `ScheduleWakeup`, which `/loop` paces itself with, until it's due, or a
`CronCreate`, for as long as it runs. What's lost is what the agent held only in memory: a prompt half typed,
or a mode switched to in it.

A terminal is stopped the same way only if you ask, with `stop_idle_terminals = true`: a shell at its prompt
with nothing at all running under it, not even a job in the background or a prompt's helper like
powerlevel10k's `gitstatusd`. It comes back in the directory it was in, showing what it showed, but what was
set in the shell, its variables, functions and aliases, is gone, and a shell takes little, so it's off unless you
turn it on.

You can have crystal keep a Claude Code started and waiting, so that a new session starts at once: with
`warm_agent = true` under `[sessions]`, the TUI has the daemon keep one where its selection rests, started as
the new-session panel would start it there, crystal's notes and all, and a new session started the same way, in
that directory with that command, takes it over, its task typed in as its first prompt. It takes what an idle
Claude Code does, a few hundred MB, for as long as a TUI is open, the resources view showing it beside crystal's own,
and it's started again every ten minutes while it waits, so that what it was told as it started, the worktree's
handoff notes and the project's memory, stays fresh. It's let go once no TUI has asked for it in a quarter of
an hour, once the setting goes off, and before a handover. What the project's memory tells it goes by its
command alone, not by its task's words, which a session started cold has searched for too; a session on a pull
request or an issue, with acceptance criteria, in the background or with another agent starts as ever. It's
off unless you turn it on.

## Notifications

When a session comes to need you while you're looking elsewhere (its agent asks you something, or finishes a
turn nobody was watching), crystal shows a desktop notification, like "claude-2 is waiting on you · app
fix/login". You're told once each time a session comes to need you, and never about a session you're watching:
one shown in the TUI counts as watched only while the TUI's terminal has the focus, as most terminals say. `u`
in the TUI takes you to it, and `U` [lists everything](events.md#timeline) that needs you.

On macOS, crystal uses [`terminal-notifier`](https://github.com/julienXX/terminal-notifier) when it's
installed (`brew install terminal-notifier`), and macOS's own notifications otherwise; on Linux, `notify-send`.
A notification server that reads markup in a notification's text (it says `body-markup` when crystal asks it,
once, with `gdbus` or `dbus-send`) gets the text with its `&`, `<` and `>` escaped, so a branch or an agent's
message holding `<b>` or `<a href>` shows as it's written.
Clicking a notification from `terminal-notifier`, or from a `notify-send` that takes actions (libnotify 0.7.10
on), takes you to the session: the TUI you used last selects it, hands it the keyboard and brings its terminal
to the front (on macOS the terminal's app, on X11 its window with `xdotool`, and inside tmux its window and
pane). The click runs `crystal pane focus --raise <session>`, which you can run yourself.

Two settings, in `[notifications]`, say when to tell you; the [settings view](configuration.md#the-settings-view) changes both:

```toml
[notifications]
after_secs = 30         # only once a session has needed you this long; one answered sooner is never told
unfocused_only = true   # only while no crystal TUI's terminal has the focus
```

`crystal notify` sends a notification of your own, through the same settings: a script's `crystal notify
"deploy finished"`, or an agent's, which a click takes you back to its session (`-n <name>` names another).
`--title` gives it a title of its own in place of crystal's, or alone, with no message, is what it says, and
`--sound` plays the `request` sound (the default), the `done` one, or `none`:

```sh
crystal notify --title "Deploy" --sound done "api is out, 0 errors"
crystal notify -t "build failed" -s none
```

A `notify_command` of your own finds the title in `CRYSTAL_NOTICE_TITLE`, beside `CRYSTAL_NOTICE`.

## Sounds and the bell

A sound plays at the same moments: one for an agent asking you something, another for one that's done.
crystal plays them with `afplay` on macOS, and on Linux with the first of `paplay`, `pw-play`, `ffplay`,
`mpg123` and `mpv` that's installed; with none, there's no sound. `[sound]` in the
[settings](configuration.md) switches them off, for every agent or some, or plays files of your own:

```toml
[sound]
enabled = true              # sounds for every agent
request = "sounds/ask.mp3"  # your own, for an agent that asks you something; from the config's directory
done = "~/sounds/done.wav"  # and for one that's done

[sound.agents]
codex = false               # not for Codex, say because it plays its own
```

`CRYSTAL_NO_SOUND=1` keeps crystal quiet whatever the config says.

A program in a session that rings the terminal's bell (`printf '\a'`, `tput bel`, a build that's done) rings
yours: `crystal attach` and the TUI's panes pass the bell of the session they show on to your terminal, which
beeps, flashes or marks its tab, the way you set it up to. A session out of sight that rings gets a `♪` before
its time in the sidebar until you look at it, rings your terminal from the TUI too, and the event log gets a
`session.bell`. A program ringing over and over rings yours at most twice a second.

## Resources

`#` shows the memory and CPU each session takes: its program and every process under it, since an agent runs node
workers, shells and MCP servers of its own, the biggest first, with how many processes that is and a bar of its
share of the whole; `s` puts the busiest first instead, and back. Under them is what crystal takes itself: the
daemon, where memory's models run; each program it runs that isn't a session's, those of one name together,
like the distiller's `claude -p`, git or a plugin's hook; and the TUI, with what it runs. Then comes the agent
[kept warm](#archiving-and-idle-agents) while there's one. The heading says all of it, with its share of the
machine's memory, and its CPU, with its share of the machine's cores. `Enter` goes to the session the bar is on.
The footer shows all of it beside `? keys` while the sidebar has the keyboard, like `2.1G 35%`, in the room its
keys leave, never in a key's place: its CPU goes first where there's less, then all of it. A click on that opens
the view. `crystal usage` prints the same table for a script, and `--json` the daemon's look with its
totals.

A process's memory is what it has in RAM now. On a Mac that's its physical footprint, what Activity Monitor's
Memory column shows, which counts what it has on the GPU, as the daemon has memory's models on Metal; on Linux
it's its resident set, so what processes share is counted in each. Its CPU is a rate, in percent of one core,
`100%` being one core busy and a machine of ten cores busy through and through `1000%`: the CPU time it had,
user and system, since the daemon looked before, over the time between. That's not `ps`'s `%cpu`, which is an
average over the process's whole life on Linux and a decaying one on a Mac. The daemon looks, with `/proc` on
Linux and libproc on a Mac, where seven hundred processes take a millisecond, every second while the view is
open and every five seconds otherwise; a look less than half a second after another counts from the one before
that, and the first in ten seconds waits half a second to have something to count from. A process that started
since the look counted from counts all its CPU time.
