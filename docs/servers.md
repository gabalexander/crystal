# Other machines and servers

<sub>[← README](../README.md#documentation)</sub>

crystal's TUI on a machine you reach over ssh, and servers: daemons of their own, each with its own sessions
and state.

- [Other machines](#other-machines)
- [Servers](#servers)

## Other machines

`crystal ssh` runs crystal on another machine through your own `ssh`, so your `~/.ssh/config`, keys and agent
work as they always do, and crystal never sees a password or a key:

```sh
crystal ssh box                                  # the TUI over there
crystal ssh me@box.example.com ls                # or any crystal command
crystal ssh box new -d -n fixer claude "fix the flaky test"
crystal ssh --install box                        # install or upgrade crystal there without asking
```

Everything after the machine goes to crystal over there as you typed it, quotes and all. That crystal has
its own daemon and sessions, which keep running when you disconnect. crystal looks for itself there on the
PATH, then in `~/.local/bin` and `~/.cargo/bin`. If it isn't there, crystal offers to install it with the
install script; if it's another version, crystal says so and offers to upgrade it. Away from a terminal it
never installs anything unless you pass `--install`. `CRYSTAL_SSH` names a command to use in place of `ssh`.

## Servers

A server is a daemon of its own, with its own sessions, tabs, layouts, backlog, tasks and memory: one for work
and one for a side project, say, or one to try something in without touching your own. `--server` (or `-L`, as
in tmux) names one for any command, the TUI included, and starts it the first time it's needed;
`CRYSTAL_SERVER` names one for every command run where it's set. Without either, crystal uses the default
server, the one it always has.

```sh
crystal --server side                       # the TUI on the server called side
crystal -L side new -d claude "try the new parser"
crystal server                              # every server: running or stopped, and its sessions
crystal server --json                       # the same, as JSON, with each one's socket and state
crystal server stop side                    # stop it and its sessions, as kill-server does
crystal server delete side                  # delete a stopped server: its sessions, tabs, backlog, memory…
```

```
$ crystal server
NAME     STATE    SESSIONS
default  running  4
side     stopped  0
work     running  2
```

A server's socket is beside the default one, named after it (`side.sock`), and its state is in
`~/.local/state/crystal/servers/side/`, so it outlives a reboot the way the default server's does. A stopped
server's sessions are those it starts again when it next starts: none after `stop`, those that were running
after a crash or a reboot. The config file, its profiles and flows, and plugins are shared by every server.

A session's program is told its server in `CRYSTAL_SERVER`, and crystal run in it reaches that server unless
`--server` names another, so an agent's `crystal send` or `crystal done` goes to its own server. The TUI on a
server other than the default names it in the top bar, before the count of its sessions. A name is letters,
digits, `-` and `_`. `crystal server delete` refuses while the server is running, and for the default server.
`crystal ssh box --server side` uses the server called side over there. `-S` (or `CRYSTAL_SOCKET`) still takes
a socket anywhere else, by its path; its state is kept beside it.
