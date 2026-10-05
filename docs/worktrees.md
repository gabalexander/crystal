# Projects and worktrees

<sub>[← README](../README.md#documentation)</sub>

How crystal groups sessions by project and git worktree, makes a worktree for an agent, moves a session into
one and removes it, runs a project's own commands, and moves a worktree onto another branch.

- [In the sidebar](#in-the-sidebar)
- [Making one](#making-one)
- [Moving a session into one](#moving-a-session-into-one)
- [Removing one](#removing-one)
- [Setting one up](#setting-one-up)
- [Projects](#projects)
- [The branch switcher](#the-branch-switcher)

## In the sidebar

The sidebar groups sessions by project, then by worktree: `⌂` marks a repository's main worktree and `⎇` a
linked one, each named by its branch, or by the label it was given (`crystal worktree create --label`), then
its branch. Sessions outside any repository come last, under their directory. `w` makes its worktree in the
selected session's project, or in the repository you started `crystal` in. Killing the last session in a
linked worktree with `x`, or closing a tab with the last sessions in some (`&`), asks next whether the
worktree goes too, one question for all of them: `y` removes it, as `W` would, and any other key keeps it.
Archived sessions that ran there are counted in the question (`nothing else is in worktree fix but 2 archived
sessions, which won't start there again`): they stay in the archive, but couldn't start there again.
`crystal kill` asks the same at your terminal; with nobody there to ask, it keeps the worktree and says how to
remove it, and `--remove-worktree` or `--keep-worktree` answers for it. `remove_emptied` under `[worktrees]`
in the [settings](configuration.md) skips the question: `"always"` removes the worktree without asking, unless
archived sessions ran there, and `"never"` keeps it. A worktree with changes you haven't committed is asked
about again before they're lost, either way. The main worktree and Claude Code's own are never asked about.

A linked worktree with no sessions left stays at the end of its project, with a `· no sessions` row under it,
until it's removed: it's still on disk, maybe with work in it. Select it, and `n` or `Enter` starts something
there, `d` and `p` show its changes and files, and `W` removes it. It shows in every tab its project has
sessions in. One made or removed outside crystal comes or goes within a few seconds.

A worktree in the middle of a rebase, a merge, a cherry-pick or a revert, stopped on conflicts say, says so
after its branch, `⎇ fix-login · rebasing`, until it's finished or aborted. A rebase detaches HEAD meanwhile,
but the line keeps the branch being rebased, and its pull request with it. Claude Code makes worktrees of its
own for its subagents, under `.claude/worktrees` in the project, and leaves one behind once it holds a change.
Those come last in their project, named `claude` and the subject of the commit each is at, `⎇ claude · feat:
add the thing`, since their branches are hashes. `Enter` starts a session in one and `W` removes it, as with
any worktree. Better, crystal tells Claude Code to run several fixes as sessions of crystal's instead: see
[agents driving agents](driving.md).

A [project](#projects) with no sessions at all stays too: after those with sessions, in every tab, its main
worktree with a `· no sessions` row under it, and its linked worktrees after. `n` or `Enter` there starts
something in it, and `W` takes it off the list, which changes nothing on disk.

## Making one

`crystal new -w <branch>` makes the worktree beside the repository, in `<repo>.worktrees/<branch>`, with any
`/` in the branch made a `-`; with `directory` under `[worktrees]` in the [settings](configuration.md), in that
directory instead, each project's in a directory named after it, like `~/worktrees/app/fix-login`. A branch that does exist is checked out as it is. One that doesn't yet starts
from `origin`'s default branch, fetched first, so it's what everyone else has as `main`, not whatever your
checkout last pulled; it follows no branch of `origin`'s, so its first `git push` makes a branch of its own.
`--base <ref>` starts it somewhere else: a branch (`origin`'s copy, fetched, when it has one), a tag, a commit,
or `HEAD` for the commit you're on. `base` under `[worktrees]` in the [settings](configuration.md) does the same for
every new worktree you don't give a base, the TUI's included, and a project without that branch starts from the
default. Offline, it's `origin`'s branch as your last fetch left it, and with no `origin`, the commit you're on.
A [flow](flows.md) makes its worktree from the branch as last fetched, without fetching.

`crystal worktree create [<branch>]` makes a worktree on its own, the way `-w` does, and prints its
directory: with no branch, a new one with a made-up name, like `brave-otter`; `--base` as for `-w`; `--path
<dir>` to make it there instead; and `--label <text>`, a few words on what it's for, which the sidebar names it
by. `crystal worktree label <worktree> <text>` changes a label, and `""` takes it off; it's kept in the
worktree's own git directory, so it goes with the worktree. `crystal worktree open <worktree>` starts a
session in a worktree you have, given its branch or its directory, as `crystal new` would there: your shell,
or the command after it. `crystal worktree list` lists the project's worktrees, the main one first, with each
one's label and how many sessions run in it (`--json` for their names).

## Moving a session into one

`crystal worktree move [<branch>]` moves a session, the one it's run in unless `-n` names another, into a
worktree of its project: the one on that branch, or else a new one, made as `create` makes it. Its program
stops and starts again there, under its name, an agent in its conversation (Claude Code and Codex are told
where they are now, and to carry on). An agent in the middle of a turn moves once the turn ends, so an agent
can run it about itself: Claude Code is told to, when you ask it to work in a worktree, rather than make one
of its own, then end its turn. Its task stays open meanwhile. A [background task](tasks.md#background-tasks) moves
once its run ends: its `claude` starts again there in its conversation, and is told where it is now as a
follow-up, which carries it on. What it changed and didn't commit stays where it was. A move still to come is
carried over `crystal restart-server`, and it's written down with the sessions, so after a crash or a reboot
the session starts again in the worktree it was on its way to, told it has moved.

## Removing one

`crystal worktree rm` (or `W` in the TUI) takes the worktree's directory or its branch, refuses while a
session is still running in it, and leaves the rest to `git worktree remove`, which keeps a worktree with
changes you haven't committed. `crystal worktree rm --force` removes it anyway, and those changes with it; `W`
asks a second time, naming them, and a second `y` does the same. Sessions that had ended in it leave the list
with it: their directory is gone, so they could never start again. The daemon does the removing, and the
sidebar says `removing…` until it's done, which for a big worktree can take a while: quitting the TUI meanwhile
doesn't stop it, and neither does `crystal restart-server`. Every TUI's sidebar says so, whoever asked: another
TUI, `crystal worktree rm`, or this one before you quit and opened it again; and `W` leaves it be meanwhile.

## Setting one up

To set a new worktree up, say install its dependencies or copy in an `.env`, have a [plugin](plugins.md) run a
command on `worktree.created`, and on `worktree.removed` to tidy up after it. What a worktree has outside its
directory, a port, a database, a route, is the repository's business, so a repository can name two programs of
its own in git config instead, which the daemon runs once crystal has made a worktree, or removed one:

```sh
git config crystal.worktreeCreateHook ~/bin/worktree-setup
git config crystal.worktreeDeleteHook ~/bin/worktree-cleanup
```

`--global` names one for every repository, and a repository's own wins. They're never read from a file in the
worktree, which would run whatever a clone brought with it. Each is run as it's named, not by a shell, with the
main worktree and the worktree as its arguments, in the main worktree (a removed one has gone), and with
`CRYSTAL_HOOK` (`worktree-create` or `worktree-delete`), `CRYSTAL_WORKTREE` and `CRYSTAL_WORKTREE_BRANCH` set.
They run one at a time, in the order things happened, and only for worktrees crystal made or removed, not for
those made or removed outside it; making one starts the daemon if it isn't running. What they print goes to `worktree-hooks.log` in crystal's state directory.
One that fails, or runs past 30 seconds, when it's stopped with everything it started, is a
`worktree.hook_failed` event in the [timeline](events.md#timeline): the worktree is made, or gone, already. The
daemon runs them with its own environment, not a login shell's, so a hook that needs your `PATH` sets it.

## Projects

crystal keeps a list of the projects you work in: every git repository a session has run in, and those you
add. A project with no sessions stays in the sidebar and in the new-session panel's "start in", so you can
start something there without a terminal of your own in it. `crystal project` lists them with how many
sessions each has (`--json` for scripts), `crystal project add [dir]` adds the repository a directory is in, and
`crystal project rm [dir]` (or `W` on its row in the sidebar) takes one off the list. Its backlog, tasks and
memory stay, and it's back as soon as a session runs there again.

`+` in the sidebar adds one from the TUI: the bottom line asks for its directory, starting in the one the
selected project is in, and `Tab` finishes a directory's name as a shell does. A directory in a project already,
at its top or not, adds that project. One that isn't in a git repository is made one once you've said `y`, and
one that isn't there yet is made, then made one; crystal's projects are git repositories. The new project is
selected, ready for `n`.

A project can say how it's run and how it's opened, in `.crystal/project.toml` at the top of a worktree:

```toml
run = "npm run dev"     # runs the project, in a terminal of its own
open = "code ."         # opens the worktree, say in your editor
```

`!` in the sidebar runs the selected session's worktree's `run` command in a session of its own there, called
`run-` and the worktree's directory (`run-app`, `run-fix-login`), and leaves the keyboard where it was; `!`
again asks to stop it, and on one that has ended starts it again. `.` runs the `open` command in the worktree,
in the background, its output thrown away. Both are shell lines, run by your `$SHELL` in the worktree's
directory. A linked worktree without a file of its own uses the main worktree's, so commit the file and every
worktree has it. To keep it out of the repository, or to say otherwise for yourself, a `[[project]]` table in the
[settings](configuration.md) takes its place:

```toml
[[project]]
path = "~/code/app"     # the project's main worktree
run = "npm run dev -- --port 3001"
open = "cursor ."
```

`crystal project run` and `crystal project open` do the same from the command line, in the worktree you're in
or the one `-C` names; `crystal project run --stop` stops it. The command opens on the machine crystal runs on,
so over [ssh](servers.md#other-machines) `open` runs on the other machine.

## The branch switcher

`B` moves the selected session's worktree onto another branch, without leaving crystal. It lists the project's
branches, the one the worktree is on first, marked `●`, then the others, the latest commit first, each with how
long ago that was, then the branches on its remotes that have no branch of yours by their name. Type to filter
them, the way the file finder does; beside the list is the selected branch's last commit and what `Enter` would
do with it. `Enter` switches to it; a remote's branch, like `origin/fix-login`, becomes a branch of your own,
`fix-login`, that follows it. When nothing matches what you typed, `Enter` makes a branch by that name, from the
commit the worktree is on, and switches to it, your changes coming along.

Once it has listed the branches, the switcher fetches every remote in the background, `git fetch --all`, at most
once a minute for a worktree, and lists them again when that's done, the branch you picked still picked, so a
branch someone else pushed is there to switch to; the header says `fetching…` meanwhile, or why it couldn't.
`Ctrl+R` fetches again straight away. A fetch can't ask for a password or a passphrase, which would take the
TUI's terminal: one that needs to fails instead, and so does one that takes longer than two minutes.

When the worktree has changes not committed, the switcher stops and asks what's to become of them:

| Key | The changes are |
|---|---|
| `s` | stashed, new files too, as `crystal: main before switching to fix-login`, where `git stash list` shows them; if git won't switch, they come straight back out |
| `b` | brought along, which git does unless they're in files the other branch changes |
| `c` | committed on the branch you're leaving, new files too, with the message you type; not on a detached HEAD, where the commit would be on no branch |
| `d` | thrown away, once you've pressed `d` a second time: the changes to files git knows, unstaged first, so new files stay; if they've changed since you were shown them, it asks again |

`Enter` takes the one the bar is on, stashing at first, and `Esc` goes back to the branches. Nothing switches in
the middle of a merge or a rebase, or with conflicts, and a branch another worktree has checked out is listed but
can't be switched to, since git keeps a branch in one worktree. `Esc` while git is at it, running a commit's
hooks say, closes the switcher, and the footer says how it went. Sessions in the worktree keep running, and see
its files change. Only a project's main worktree switches: a linked one is named after the branch it was made
for, and stays on it.
