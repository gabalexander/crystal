# How it works

<sub>[← README](../README.md#documentation)</sub>

The daemon, the clients that talk to it, how it hands itself over to a new crystal, and what crystal does with
text it didn't write.

```
crystal (TUI) ─────────┐
                       ├── unix socket ──▶ crystal daemon ──┬── PTY ──▶ claude
crystal CLI ───────────┘                                    ├── PTY ──▶ codex
                                                            └── PTY ──▶ zsh
```

One binary is both the client and the daemon. The first `crystal` you run starts the daemon in the background.
The daemon owns the PTYs, tracks each session's status and saves its state to disk. The TUI and the CLI
commands talk to it over a unix socket, so closing the TUI never stops an agent.

`crystal restart-server` hands the daemon over to the crystal it's run from. The daemon finishes the requests
it's answering, gives plugins' hooks a few seconds to finish, then writes down what exec can't carry, each
session's state and screen, and runs the new crystal in its own process with `exec`. The pid stays the same,
so every program is still the daemon's child, and how it ends is still known; the PTYs, a background task's
pipes to its `claude` and the listening socket stay open across the exec, so a client that connects meanwhile
only waits. Attaches and event streams are cut, and come back by themselves: an event stream picks up after
the last event it had, with none missed, and the log has a `daemon.handed_over`. If the new crystal can't take
over, the daemon is restarted cold from the sessions it wrote down first. A worktree still being removed is left
to git: the new daemon waits for it to finish, has git try again if the worktree is still there, then answers
whoever asked.

Text crystal didn't write, like a session's name, what an agent reports, Claude's answers in a background task,
a pull request's title, a branch, a commit or a file in a preview, reaches your terminal only as text. Control
characters, the escape sequences they start (a window title, the clipboard, a link, the alternate screen) and
the bidi controls that turn text around are taken out of every frame the TUI draws, of a background task's
transcript before its screen draws it, and of what the CLI prints for you to read (`--json` is for scripts). A
session's name can't hold them. A program draws its own pane: its output goes through the terminal crystal
emulates for it, as it would through any terminal.
