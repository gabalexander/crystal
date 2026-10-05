# The command line

<sub>[← README](../README.md#documentation)</sub>

crystal's commands, for scripts and for agents: every one at a glance, then how `crystal new`, `rename`,
`respawn` and `attach` behave. `crystal --help`, and `--help` after any command, say the rest.

- [Every command](#every-command)
- [New sessions, names and attaching](#new-sessions-names-and-attaching)

## Every command

Everything is also a command, for scripts and for agents:

```sh
crystal new claude                          # start Claude Code here and attach to it
crystal new -d -n review -c ~/code/app codex   # start one in the background, named, somewhere else
crystal new -w fix/login claude             # start one in a new worktree, on a new branch off origin's main
crystal new -w spike --base HEAD claude     # the same, its branch off the commit you're on
crystal new -d -e PORT=4000 npm run dev     # with a variable set in its environment, over yours
crystal task "update the docs"              # run Claude without a terminal, in the background
crystal result task                         # a task's answer
crystal answer task y                       # allow what a task asks for: y, n or always
crystal worktree list                       # this project's worktrees and the sessions in each
crystal worktree create spike --label "try sqlite"   # a worktree on its own, its directory printed
crystal worktree open spike claude          # start a session in a worktree, by its branch or directory
crystal worktree move fix/login             # move this session into a worktree, its agent carried on there
crystal worktree rm fix/login               # remove that worktree, once nothing runs in it
crystal ls                                  # list sessions and how they're doing
crystal ls --json                           # the same, as JSON, for scripts and agents
crystal attach review                       # show a session; Ctrl+\ hands your terminal back
crystal send review "check the diff"        # type into a session and press Enter
crystal send-keys review 1                  # press keys: an answer, Enter, Escape, C-c, Up…
crystal wait review                         # block until its agent stops working; print how it ended
crystal wait t12                            # block until task t12 closes; print done, failed or cancelled
crystal read review --lines 20              # print the last 20 rows of its screen
crystal read review --history               # and what scrolled off it before
crystal read review --since 10m             # only what it wrote in the last ten minutes
crystal clear -n review                     # clear its screen and history but its prompt line
crystal ps review                           # what runs in its terminal, and where (process-info)
crystal observe review                      # its terminal as JSON lines, for a program; `control` drives it
crystal api snapshot                        # everything at once, as JSON, for a client of your own
crystal api schema --json                   # the JSON Schema of what crystal says over its socket
crystal rename review reviewer              # give a session another name
crystal respawn reviewer                    # run an ended session again; an agent in its conversation
crystal kill review                         # stop one session; asks if its emptied worktree goes too
crystal archive review                      # stop it and keep it in the archive, out of the list
crystal unarchive review                    # start it again where it was, in its conversation
crystal ls --archived                       # the archived sessions
crystal usage                               # the memory and CPU each session takes, and crystal's own
crystal usage --json                        # the same, as JSON, with the totals
crystal project                             # the projects crystal knows, running or not
crystal project run                         # run this worktree's project in a session of its own
crystal kill-server                         # stop every session, and the daemon
crystal restart-server                      # restart the daemon on this crystal, say after an upgrade
crystal restart-server --cold               # stop it and start it again: sessions start again too
crystal update                              # install the latest release, the daemons restarted on it
crystal completions zsh                     # complete crystal's commands in your shell
crystal server                              # list the servers, daemons of their own
crystal config                              # where the config file is, and the settings in effect
crystal config export ~/backups             # the settings in one file, to keep or take elsewhere
crystal config import ~/backups             # merge them in, here or on another machine
crystal guide                               # a page on what to start, the keys that matter and what agents call
crystal profile                             # list your agent profiles
crystal profile show review                 # what a profile runs, and where it starts
crystal pane split review                   # show a session in a pane beside yours in the TUI
crystal tab new review                      # a new tab in the TUI, in front
crystal title set "deploying"               # the title of the TUI's terminal, until `crystal title clear`
crystal sidebar move review --up            # put a session a place up in the sidebar, for good
crystal open docs/plan.md                   # show a file in the TUI, a markdown file as its page
crystal layout                              # the TUI's tabs and how each splits its panes
crystal layout apply dev.json               # lay them out as a file says, starting what isn't there
crystal skill --install                     # teach Claude Code to drive crystal
crystal integration install                 # hooks or plugins for the agents installed here
crystal mermaid docs/flow.md                # draw a page's mermaid diagrams as text
crystal mermaid --open docs/flow.md         # or have mermaid draw them in your browser
crystal ssh box                             # crystal's TUI on another machine
```

```
$ crystal ls
NAME    STATE     PID    PROJECT  BRANCH     DIRECTORY                       PROGRAM  COMMAND
claude  waiting   41210  app      main       ~/code/app                      claude   claude
fixer   working   41377  app      fix/login  ~/code/app.worktrees/fix-login  claude   zsh
review  exited 0  41388  app      main       ~/code/app                      codex    codex
```

Their pages say more: `task`, `result` and `answer` in [background tasks](tasks.md#background-tasks);
`worktree` in [projects and worktrees](worktrees.md), and `project` in [projects](worktrees.md#projects);
`send`, `send-keys`, `wait`, `read`, `clear`, `ps`, `observe` and `api` in [agents driving
agents](driving.md); `pane`, `tab`, `title`, `sidebar move` and `layout` in [laying out the
TUI](driving.md#laying-out-the-tui), and `skill` in [a skill for Claude
Code](driving.md#a-skill-for-claude-code); `archive` and `unarchive` in
[archiving](sessions.md#archiving-and-idle-agents); `usage` in [resources](sessions.md#resources); `update`
and `completions` in [installing crystal](install.md); `server` and `ssh` in [other machines and
servers](servers.md); `config` and `profile` in [settings](configuration.md); `integration` in [agents you
start yourself](agents.md#agents-you-start-yourself); `open` and `mermaid` in [files an agent shows
you](code.md#files-an-agent-shows-you) and [the file finder](code.md#the-file-finder-and-the-tree-browser);
and `guide` prints [the guide](guide.md).

A command whose output is cut short by what reads it, like `head -1` in `crystal ls | head -1` or `grep -m1
task.closed` reading `crystal events --follow`, stops there and exits 0, saying nothing, so a script under `set
-o pipefail` goes on; `crystal update`, `crystal integration install` and a plugin's build finish what they
started first.

## New sessions, names and attaching

`crystal new` with no command starts your shell: the one `default_shell` under `[terminal]` in the
[settings](configuration.md#terminals-the-window-and-the-tab-bar) names, or else `$SHELL`, as a login shell on a Mac. `--env
KEY=VALUE` (`-e`), as many times as you like, sets a variable in the session's environment over the one it
would have from yours: `-e PORT=4000`, `-e DEBUG=` for an empty one. crystal's own, like `TERM` and
`CRYSTAL_SESSION`, can't be changed.

`crystal rename` changes what a session is called; its program and its saved place after a restart follow the
new name. `crystal respawn`, or `Enter` on an ended session in the TUI, runs its command again in the same
directory, under the same name and in the same place in the list, with your environment; on one that couldn't
start again after a restart, it tries again. Claude Code and Codex
come back in the conversation they were in, without being asked their task again. Every session's program also gets `CRYSTAL_SESSION_ID`, which stays the same
when the session is renamed, while `CRYSTAL_SESSION` keeps the name the program started under.

The first `crystal new` starts the daemon. Sessions keep running after you detach or close the terminal, and
`crystal attach` picks up exactly where the screen was. With no name it attaches to the newest session; on a
session that has ended, it prints the last screen and how the program exited.

In `crystal attach` a few keys are crystal's, as they are in a pane: `Ctrl+\` detaches, and the prefix, `Ctrl+B`,
then `v` puts the screen in [copy mode](tui.md#zoom-copy-mode-and-search), with the search typed on its bottom row and
what it says at its top right. `PageUp` or `PageDown` after the prefix, or `Shift+PageUp` and `Shift+PageDown`,
page through the session's history, from before you attached as well, until you type. The prefix twice sends
the program the prefix, and while it waits, the top right says what can follow it. They're the keys `[keys]`
gives `prefix`, `copy`, `page-up` and `page-down`, and one written `direct+` works without the prefix.
