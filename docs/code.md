# Diffs, files and pull requests

<sub>[← README](../README.md#documentation)</sub>

What a session's worktree changed, its files, and its project's pull requests and issues, each a view over the
panes, a key from the sidebar.

For a project on GitHub or GitLab, each worktree line shows its branch's pull request, `#57` (a merge request,
`!57`, on GitLab), with a mark for what matters most about it, `merged` once it has; the tab bar counts the
pull requests and issues open on the selected session's project, and a click on either count lists them; `o`
opens its pull request in your browser, `O` lists the project's pull requests and `i` its issues: see [pull
requests and issues](#pull-requests-and-issues).

- [The diff](#the-diff)
- [Pull requests and issues](#pull-requests-and-issues)
  - [Pull requests](#pull-requests)
  - [Issues](#issues)
  - [On GitLab](#on-gitlab)
- [The file finder and the tree browser](#the-file-finder-and-the-tree-browser)
- [Find in files](#find-in-files)
- [Files an agent shows you](#files-an-agent-shows-you)

## The diff

`d` shows what changed in the selected session's worktree, the way VS Code and GitHub show it: the changed
files on the left, each with its status (`M`odified, `A`dded, `D`eleted, `R`enamed, `U`ntracked) and how many
lines it adds and removes, and the selected file's diff on the right, with both files' line numbers, added lines
on green, removed lines on red, and the words that changed inside a line marked stronger.

It starts with what isn't committed yet, staged or not, new files included. `b` switches to the whole branch
since it left its base (where it meets `origin`'s default branch, or `main` or `master`): everything an agent
committed, as its pull request would read. From [the pull requests](#pull-requests), `Ctrl+D` shows one's own
diff here, as GitHub or GitLab has it, with no worktree needed.

Once you've read a file, `r` marks it reviewed: it sinks to the bottom of the list with a `✓`, and the next file
is selected, so `r` after `r` reads through a change. A mark is crystal's own bookkeeping, nothing staged or
committed, and it keeps until the file changes again, or HEAD moves, a commit say, when the next diff is new work
to read. They're kept with your tabs, in crystal's database, apart for each worktree's uncommitted changes and
its branch, and for each pull request's diff, where only a file changing takes a mark off.

`t` folds the list into a tree of directories, and back, and crystal remembers which you like. Every directory
starts open, a directory that holds only another reads as one row with it, like `src/tui`, and a directory's
row shows how many lines change under it, and a `✓` once every file under it is reviewed; selected, it lists them.

`/` filters the list as you type, the way the [file finder](#the-file-finder-and-the-tree-browser) matches: a few
letters of a file's path, in order (`rfnd` keeps `src/billing/refund.rs`), and the header counts how many
match. In the tree, it keeps the files that match and the directories they're in, all open. `↑` / `↓` still
move through the files while you type; `Enter` keeps the filter and gives the keys back to the list, and `Esc`
clears it, there or once you're back in the list, before a second `Esc` closes the diff.

| Key | In the diff |
|---|---|
| `j` / `k`, `↓` / `↑` | the next or previous file |
| `Space` / `Shift+Space`, `PageDown` / `PageUp` | page through the file's diff |
| `]` / `[` | the next or previous hunk |
| `v` | side by side, the old file beside the new one, or unified again; side by side needs 120 columns |
| `b` | the branch since its base, or the uncommitted changes again |
| `r` | mark the file reviewed, or take its mark off |
| `t` | the files as a tree of directories, or a list again |
| `←` / `→`, `h` / `l` | in the tree: fold a directory or go up to the one it's in; open one or go into it |
| `Enter` | in the tree: fold or open the directory |
| `/` | filter the files by a few letters of their paths: `Enter` keeps the filter, `Esc` clears it |
| `Esc` / `q` | clear the filter, if there's one; back to the sidebar |

The wheel scrolls the diff, and moves through the files over the list; a click on a directory folds or opens it.

## Pull requests and issues

For a project whose remote is on GitHub or GitLab, each worktree line shows its branch's pull request when
there is one, open or merged lately: `#57`, or on GitLab, where it's a merge request, `!57`, and a mark for what
matters most about it, in this order:

| Mark | Meaning |
|---|---|
| `merged` | it has merged: the worktree's work is in, and the worktree can go |
| `conflicts` | its branch conflicts with the one it would merge into, so it can't merge as it stands |
| `✗` | a check failed |
| `±` | a reviewer asked for changes |
| `draft` | it's still a draft |
| `◌` | checks are still running |
| `✓` | approved |

A pull request that's simply ready shows its number alone. `o` opens the selected session's pull request in your
browser. On the right of the [tab bar](configuration.md#terminals-the-window-and-the-tab-bar), after the sessions, are how many
pull requests and issues are open on the selected session's project, `3 prs · 5 issues` (`mrs` on GitLab):
none says nothing, a list as long as the forge gives at once counts `100+`, and a narrow terminal leaves them
out before the tabs, or what you have the bar show at its right, give way. A click on `3 prs` lists them, as
`O` does, and on `5 issues`, as `i` does.

crystal asks the forge's own command line tool, [`gh`](https://cli.github.com) for GitHub and
[`glab`](https://gitlab.com/gitlab-org/cli) for GitLab, as soon as it sees a project and then once a minute for
its pull requests (the open ones, and the last 20 merged) and every five minutes for its issues, so your login
works as it always does and crystal never sees a token. Which forge a project is on comes from
its `origin` remote (or its first, without one): `github.com`, or a host `gh` is logged in to, like a GitHub
Enterprise, is GitHub; `gitlab.com`, or a host in glab's config, like a GitLab of your own, is GitLab.
Without the tool, or logged out of it, or for a project on neither, nothing is shown; `o`, `O` and `i` say why.

### Pull requests

`O` lists the open pull requests of the selected session's project, with each one's author and branch, whether
it's a draft or conflicts with its base, how its checks stand and what its reviewers decided, then, below them
and muted, the ones merged lately, marked `merged`. Under the list is the selected one, read whole: who wants to
merge which branch into which, or merged it, whether it can merge as it stands, each of its checks, its
description, and then its conversation, comments and reviews in the order they came. Typing filters the list by
number, title, author or branch; `↑` / `↓` pick another, and `PageUp` / `PageDown` scroll what's under the list.
The list opens on what the forge said last, and `Ctrl+R` asks again, the heading saying so until it answers.

`hide_draft_prs` under `[forge]` in the [settings](configuration.md), or `hide drafts` in the [settings
view](configuration.md#the-settings-view), leaves drafts out of the list, the tab bar's count and what `/` finds, for what's
asking to be reviewed; the heading says how many it hides. A worktree's own pull request shows on its line all
the same.

| Key | In the pull requests |
|---|---|
| `Enter` | open the [new-session panel](sessions.md#starting-a-session) in the pull request's worktree, with the task `Work on pull request #57: <its title> (<its address>)`, its agent told to read it first and keep to it, as [`--pr`](tasks.md#tasks) tells it; not on one that has merged |
| `Ctrl+D` | its whole diff, in [the diff](#the-diff); `Esc` comes back to the list |
| `Ctrl+C` | comment on it: `Enter` posts, `Alt+Enter` breaks a line, `Esc` puts the comment away |
| `Ctrl+O` | open it in your browser |
| `Ctrl+R` | ask the forge for the list again, and the selected one with it |
| `Esc` | close the list |

A pull request's worktree is the project's worktree on its branch, when there's one already; otherwise crystal
fetches the branch from `origin` and makes one beside the others, like `app.worktrees/fix-login`, its branch
following `origin`'s so `git pull` brings what's pushed later. A pull request from a fork isn't on a branch of
the project's, so it's fetched from where the forge keeps it (`refs/pull/57/head`, or
`refs/merge-requests/57/head` on GitLab) onto a branch named for its owner, like `ana/main`, the way `gh pr
checkout` names it, so that a fork's `main` is never taken for yours. GitLab doesn't name a fork's owner, so
there it's `mr-57/main`.

A comment is posted as you, the way `gh pr comment` or `glab mr note` would. While it's on its way the box
waits; once posted, the pull request is read again with it, and if the forge refuses it, it's all still in the
box, with why.

### Issues

`i` lists the open issues of the selected session's project, the latest to change first, with the selected
issue under the list: its text, then what's been said on it. Typing filters them by number, title, label or
author. It opens on the issues listed last, and `Ctrl+R` asks the forge again.

| Key | In the issues |
|---|---|
| `Enter` | open the [new-session panel](sessions.md#starting-a-session) on a new worktree with a branch named after the issue, like `42-fix-login-redirect`, and the task `Fix issue #42: <its title> (<its address>)`, its agent told to read it with `gh issue view 42` or `glab issue view 42` first, as [`--issue`](tasks.md#tasks) tells it |
| `Ctrl+C` | comment on it: `Enter` posts, `Alt+Enter` breaks a line, `Esc` puts the comment away |
| `Ctrl+E` | change its title and text: `Tab` goes between them, `Enter` saves both, `Esc` keeps them as they were |
| `Ctrl+O` | open it in your browser |
| `Ctrl+R` | ask the forge for the list again, and the selected one with it |
| `Esc` | close the list |

A title and text you give an issue stay, even when the forge answers a list or a read it was asked for before
it saved them, which may not have them yet; what it says after that goes, should someone change them again.

### On GitLab

Everything above works on a GitLab project, through `glab`, with merge requests where GitHub has pull requests.
GitLab's list of merge requests doesn't say how their checks or reviews stand, so their worktree lines and rows
show only `merged`, `conflicts` and `draft`; reading one shows its pipeline as its check, and an approval in its
conversation. GitLab wants
a login to read comments, even on a project anyone can see: logged out, a merge request or an issue reads
without them.

## The file finder and the tree browser

`p` finds a file in the selected session's worktree, like an editor's quick open: type a few letters of its path,
in order (`rfnd` finds `src/billing/refund.rs`), and the best matches come first, with the selected one
previewed beside the list. Letters in a file's name, at the start of a word, or next to each other count for
more. `↑` / `↓` pick another, `Enter` opens it in your `$EDITOR` (or `vi`) as a session of its own in that
worktree, named after the file, and `Esc` closes the finder. Files git ignores aren't listed.

`E` shows the same files as a tree: directories first, each folded until `→` opens it, and the selected file
previewed on the right. Whatever you type filters the tree, as the finder matches, down to the files whose
paths match and the directories they're in, all open, with the best match selected. Drag the line between the
tree and the preview to make the tree wider or narrower.

| Key | In the tree browser |
|---|---|
| `↑` / `↓` | the file or directory above or below |
| `→` / `←` | open a directory, or go into one that's open; fold it, or go up to the directory a file is in |
| `Ctrl+←` / `Ctrl+→`, `Alt+←` / `Alt+→` | a word back, or on, in what's typed |
| `Ctrl+A`, `Ctrl+Home` / `Ctrl+End` | to the start, or the end, of what's typed |
| `Enter` | open or fold a directory; read a file into the preview again, with whatever an agent changed since |
| `Space` / `Shift+Space`, `PageDown` / `PageUp` | page through the preview |
| `Home` / `End`, `Shift+↑` / `Shift+↓` | the top or end of the preview; a line up or down |
| `Ctrl+E` | open the file in your `$EDITOR`, the way the finder's `Enter` does |
| `Ctrl+Y` | copy the path, from the top of the worktree |
| `Ctrl+R` | a markdown file's source, or its page again |
| `Esc` | clear what's typed; then close |

The wheel scrolls the preview, and moves through the tree over it.

Both preview a file highlighted, with its lines numbered: comments, strings, numbers and keywords, in Rust,
JavaScript and TypeScript, Python, Go, C and C++, Java, Kotlin and Swift, Ruby, shell, SQL, TOML, YAML, JSON,
CSS and Dockerfiles. A binary file says so; of a long one, the first megabyte or 10,000 lines are shown.

A markdown file shows as the page it makes: headings, lists, emphasis, quotes and GitHub's alerts, tables in
aligned columns, links with their address beside them, and fenced code highlighted. `Ctrl+R`, in the finder
too, flips it to its source and back, and that holds from one file to the next. A ```` ```mermaid ```` fence is
drawn as the diagram, in boxes and arrows, with a caption under it saying what kind: sequence diagrams,
flowcharts, and state, class and ER diagrams. Any other kind, or one too wide for the preview, stays its source,
and the caption says why. What Claude says in a [background task](tasks.md#background-tasks)'s transcript is a page in
the same way.

`crystal mermaid` draws a diagram on the command line the same way, from a file or standard input: a diagram, or
each mermaid fence of a markdown file, as wide as the terminal (`--width` gives another width) and in box drawing
(`--ascii` in ASCII). One that can't be drawn is printed as it is, and the command fails saying why, so an agent
can check a diagram before it writes it into a page. `mermaid_ascii = true` in the [settings](configuration.md) draws
every diagram in ASCII, `+ - | > v`, in previews, transcripts and `crystal mermaid` alike, for a font or a
terminal without box drawing.

`crystal mermaid --open` has mermaid itself draw them instead, in your browser: for the exact picture, curves and
all, and the kinds a terminal can't draw. The diagrams go on a page in crystal's state directory
(`~/.local/state/crystal/diagrams/`), named for what's on it, so the same diagrams are always the same file; the
command prints its path and opens it, or over ssh puts its link on your clipboard. The page loads mermaid from
the jsDelivr CDN, so it needs the network the first time, and follows your system's light or dark.

```
$ printf 'sequenceDiagram\n  Alice->>Bob: hello\n  Bob-->>Alice: hi\n' | crystal mermaid
┌───────┐  ┌─────┐
│ Alice │  │ Bob │
└───┬───┘  └──┬──┘
    │  hello  │
    ├────────▶│
    │   hi    │
    │◀┄┄┄┄┄┄┄┄┤
    │         │
```

## Find in files

`G` searches the files of the selected session's worktree for what you type, with `git grep`: the files git
tracks and the new ones it would, but not those it ignores, or binary files. It searches once you've typed two
letters and stopped for a moment, the case of letters counting only when what you type has a capital in it. The
lines it finds are listed under their files, the first 500 of them, and the lines around the selected one are
beside the list. `↑` / `↓` pick another, `Enter` opens the file in your `$EDITOR` at that line, as a session of
its own like the file finder's, and `Esc` closes it. The line goes to your editor the way it takes one: `+12` for
most, `file:12` for Helix, Zed and Sublime Text, and `--goto file:12` for VS Code, Cursor and their kind.

## Files an agent shows you

Ask an agent to show you a file, "open it" or "show me the plan", and it runs `crystal open <file>...`: the
files come up in the TUI you used last, in a view of their own over the tabs, rather than pasted into its
answer. They're listed on the left, each by its path from the worktree they were opened in, and the one selected
is read on the right, highlighted as the [file finder](#the-file-finder-and-the-tree-browser) shows it, a
markdown file as its page with its mermaid diagrams drawn. That makes it the place to read an explanation an
agent writes you: a markdown page with a diagram for each flow, which it opens once it's written.

| Key | In the files shown |
|---|---|
| `↑` / `↓`, `Tab` / `Shift+Tab` | the file above or below; `Tab` goes round |
| `Space` / `Shift+Space`, `PageDown` / `PageUp` | page through the preview |
| `Home` / `End`, `Shift+↑` / `Shift+↓` | the top or end of the preview; a line up or down |
| `Ctrl+R` | a markdown file's source, or its page again |
| `Enter` | open the file in your `$EDITOR` (or `vi`) as a session of its own, as the finder's `Enter` does |
| `Esc` | close it |

A click picks a file, and the wheel scrolls the preview. The files take the place of any view that was open,
like the diff, and files opened again take the place of those shown, read afresh; what else was open, like the
new-session panel with what you'd typed in it, is there again once they're closed.

A [profile](configuration.md#profiles) can ask for a page every time, like docket's explainer:

```toml
[[profile]]
name = "explainer"
description = "Writes you a page on part of the code"
agent = "claude"
prompt = "Explain this part of the codebase:"
postfix = """Change no code. Write the explanation to a markdown file: a summary, then the flow with \
file and function names, a mermaid diagram for each flow or structure. Then crystal open it, \
and keep your answer here short."""
launch = "session"
```

Claude Code is told to open a file only when you ask, in a prompt or a profile like that one, since it takes
your screen: a file it wrote or changed is no reason, and it names the path instead. `crystal open` takes text
files only, from the directory it runs in or absolute, and refuses anything else, an image, a PDF or another
binary, before anything is shown, so the agent names that path too. With no TUI open, nothing is shown and it
fails, saying so. You can run it yourself, from any shell.
