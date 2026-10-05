# Roadmap

<sub>[← README](../README.md#documentation)</sub>

What crystal does so far, in the order it came.

- [x] Project skeleton
- [x] Daemon that runs an agent in a PTY and keeps it alive
- [x] Attach and detach
- [x] Session list in a sidebar
- [x] Session status from Claude Code's hooks
- [x] Session status from the screen, for agents without hooks
- [x] Rules for reading each agent's screen in files you can change, and why a session reads as it does
- [x] Projects and worktrees
- [x] Resume after a restart
- [x] Restarts after a crash that start agents a moment apart and keep what can't start, saying why
- [x] Split panes
- [x] Tabs
- [x] Agents that start, message, wait on and read other agents
- [x] Tasks that close done or failed, and a backlog per project
- [x] Plugins: crystal's own switched on and off, and your own actions, panes and hooks
- [x] Links in panes, opened with `Ctrl`+click or by a plugin; plugins' builds and startup commands
- [x] Plugins a project ships, on for it alone; plugin panes in popups, splits and tabs, opened from the
  command line; and example plugins
- [x] An event log, a stream of events on the socket, and waits on it
- [x] Any agent saying what it's doing and how to resume it, and sessions named from their first prompt
- [x] Restart the daemon on a new crystal without stopping its sessions
- [x] Hooks for a Claude Code or Codex typed into a shell, resumed after a restart, and subagents counted
- [x] Hooks or plugins for 15 more agents, each resumed in its conversation after a restart
- [x] Archived sessions, idle agents stopped, right-click menus, and projects kept with their run and open commands
- [x] A sidebar in a stable order, or one of your own, and its rows laid out your way, with what agents report
- [x] Idle sessions stopped by default, sparing work they left running, and back as you go to them; an agent
  kept warm for the next session
- [x] Files' paths in panes opened in your editor at their line, and a key back to the session you were on
