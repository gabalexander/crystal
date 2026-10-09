# The wiki

<sub>[← README](../README.md#documentation)</sub>

A project's wiki is one long page about its code, in your browser: an outline of its parts at the left, each
part's diagrams and prose in the middle, with links into the code at the very lines, and a chat at the right
that answers questions about the code. `crystal wiki build` writes it, for the commit the project is at, into
crystal's state; `crystal wiki open` shows it; `crystal wiki export` writes it out as a site of its own. It's
the `wiki` plugin, on unless you [switch it off](plugins.md#switching-them-on-and-off).

- [Reading it in the browser](#reading-it-in-the-browser)
- [Asking about the code](#asking-about-the-code)
- [Exporting it as a site](#exporting-it-as-a-site)
- [In the TUI](#in-the-tui)
- [The server's API](#the-servers-api)

## Reading it in the browser

```sh
crystal wiki open                 # this project's wiki in your browser
crystal wiki open -C ~/code/app   # another project's
crystal wiki serve                # serve every project's wiki here, until ctrl+c
crystal wiki serve --port 8080 --open
```

`crystal wiki open` opens your browser on the wiki of the project it's run in, or on the list of every wiki the
server has when the project has none yet. It needs a server to serve them, and starts one in the background the
first time: `crystal wiki serve` cut loose from your terminal, which goes on serving every project's wiki until
crystal's daemon stops, and is found again by the next `open`. After an upgrade, `open` stops the one the crystal
before started and starts its own. What it says is in `serve.log` in the server's wiki directory.

`crystal wiki serve` runs one in the foreground instead, printing where each wiki is, until you stop it. It
listens on 127.0.0.1 only, on port 7347, or on a free port when that's taken; `--port` names one, and `--open`
opens the browser too. Each crystal [server](servers.md) has its own wikis, and its own server for them.

Over ssh, the browser is on your machine and the wiki on the other, so `open` prints the address and the line
that forwards its port, rather than opening anything:

```
$ crystal wiki open
crystal runs on another machine: forward the port from yours, then open the link there:
  ssh -L 7347:127.0.0.1:7347 box
http://127.0.0.1:7347/p/app-1f6c0e8a9b2d4c11/
```

A link into the code opens the file at its line in your editor: `$VISUAL`, or else `$EDITOR`, or `vi`. An editor
with a window of its own, like VS Code, Cursor, Zed or Sublime Text, starts as it is; one that runs in a
terminal, like Vim or Helix, starts in a session of its own in the project, as the TUI starts one for a file,
brought to the front in the TUI you used last. A link held with the modifier opens the file on GitHub or GitLab, at the
commit the wiki was written at, when the project has a forge. Only files in the project's repository open.

The page draws its diagrams with [mermaid](https://mermaid.js.org), which crystal downloads the first time a page
asks for it, at a pinned version whose SHA-256 it checks, into its cache (`~/.cache/crystal/wiki/`) beside
memory's models. With `CRYSTAL_NO_MERMAID_DOWNLOAD` set, it never downloads it, and a page with no mermaid shows
each diagram's source instead.

The server answers only to `127.0.0.1` and `localhost` (a request naming any other host is refused, which keeps
a site that points its name at your machine out), and asks a question or opens a file only for its own pages,
by what the browser says a request came from.

## Asking about the code

The chat at the right of a wiki answers a question about the project's code. Each question runs Claude Code
headless (`claude -p`) in the project's main worktree, told the wiki's outline and the part you're reading, and
to link each file, type and function it names to the lines it read; its answer streams in as it writes it,
with the files it reads as it reads them, and what it cost at the end. A follow-up carries the conversation on.

It may only read: `Read`, `Grep`, `Glob`, `git log` and `git ls-files`, and nothing else, without asking. It runs
with no MCP servers, nobody's hooks and none of the project's settings, and at most the budget a question has.
Closing the page stops a question still going.

```toml
[wiki]
ask_model = "sonnet"     # the model that answers, as `claude --model` takes it
ask_budget_usd = 0.5     # the most a question may spend
```

Both are in the [settings view](configuration.md#the-settings-view)'s Tasks tab too, and count from the next
question.

## Exporting it as a site

```sh
crystal wiki export site               # this project's wiki, as a site in ./site
crystal wiki export site -C ~/code/app
```

An export writes the wiki out as a site of its own: `index.html`, with the wiki in it, so it works opened from
its file; `wiki.json` beside it; and the page's files and mermaid in `assets/`. Put the directory on any static
host, like GitHub Pages (the `.nojekyll` file in it keeps Pages from leaving files out). Links into the code go to
the forge, at the wiki's commit, or are plain code without one; and the chat says that asking needs `crystal
wiki serve`. Without mermaid, which can't always be downloaded, the export says so and leaves the diagrams as
their source.

## In the TUI

`X` lists the wiki with crystal's own plugins, and its actions under it: build the project's wiki, update it for
what changed, and open it in the browser. `Enter` on one runs it in the background, for the project of the
session you're on, and the footer says how it went, the wiki's address for `open`; `crystal plugin log wiki`
has the rest of what it said.

## The server's API

What the page asks the server, for a page of your own. `<key>` is a wiki's directory, a project's name and a
hash of its path.

| Request | What it answers |
|---|---|
| `GET /` | the wikis: a page for a browser, or JSON for `fetch` (each one's `key`, `name`, `root`, `commit`, `branch`, `updated` and `url`) |
| `GET /projects.json` | the same JSON |
| `GET /p/<key>/` | the wiki's page |
| `GET /p/<key>/wiki.json` | the wiki |
| `GET /assets/<file>`, `GET /p/<key>/assets/<file>` | the page's files, and `mermaid.min.js` |
| `GET /p/<key>/api/status` | `{"building", "progress", "updated", "stale"}`: a build going on and how far it is, when the wiki was written, and whether its branch has moved on since |
| `GET /p/<key>/api/open?path=…&line=…` | opens the file at the line in your editor: 204, or 404 for a file that isn't in the repository |
| `POST /p/<key>/api/ask` | `{"question", "conversation", "section"}`, answered as `text/event-stream`: `delta` events with the answer's `text`, a `tool` event for each tool it uses (`name`, and the `path` it reads), then `done` with the `conversation` to follow up in and its `cost_usd`, or `error` with a `message` |

A question and an open need the page's own origin: a POST says it (`Origin`), and one from another site is
refused.
