# Memory

<sub>[← README](../README.md#documentation)</sub>

What a project's sessions learn, kept for the next ones: how you and agents add to it and search it, what an
agent is shown as it starts, search by meaning, and the distiller that reads what a session did.

- [Remembering](#remembering)
- [What an agent is shown](#what-an-agent-is-shown)
- [Through MCP](#through-mcp)
- [Search by meaning](#search-by-meaning)
- [The distiller](#the-distiller)

## Remembering

A project keeps a short list of what its sessions have learned, so the next session doesn't learn it again: a
decision and why it was made, a gotcha, a command that works, or a note. You or an agent in a session add to
it:

```sh
crystal remember "Fees are kept in cents; never store a float"
crystal remember -k gotcha -f tests/ledger.rs "The ledger tests need the database up: make db"
crystal remember -k command "make e2e runs the browser tests; they take about 4 minutes"
crystal memory add -k decision --title "Refunds go through the ledger" "Never call the processor directly: ..."
crystal memory                       # the list, newest first; `memory list -k gotcha` keeps to a kind
crystal memory search ledger tests   # the entries that have most to do with those words
crystal memory search db -k command -f src/ledger --fresh -n 5   # of a kind, about those files, none stale
crystal memory show 3                # one in full: its files, where it came from, how often it was said
crystal memory export > MEMORY.md    # the whole list as markdown
crystal memory rm 3                  # forget one, or `rm 3 5 8` several
crystal memory list --forgotten      # what was forgotten, the latest first
crystal memory list --expired        # the notes and outcomes nobody found again
crystal memory list --status         # the entries that read as status rather than lessons
crystal memory rm --status           # what that would forget; `--yes` forgets them
crystal memory kind gotcha 12 15     # make entries gotchas: what they say stays as it is
crystal memory kind --notes          # the notes a model reads as lessons, and their kinds; `--yes` makes them so
crystal memory promote 2             # copy one into the project's CLAUDE.md, under "Notes"
crystal memory distill fixer         # have a model read what a session did, now
crystal memory embed                 # download the model that searches by meaning
crystal memory dedupe                # entries that say what another does, by meaning; --apply merges them
```

- `-k` is `decision`, `gotcha`, `command` or `note` (the default). Inside a session, an entry goes to the
  session's project and says which session added it; elsewhere it goes to the project of the current
  directory, or of `-C <dir>`. `crystal memory add` is the same as `crystal remember`.
- An entry's first line is its title: what the list, a search and an agent starting are shown of it, and
  `show` gives the rest. `--title` gives it one of its own, a line of 120 characters at most, over what it
  says.
- How a task turned out isn't kept here: its project's [tasks](tasks.md#tasks) keep that (`crystal tasks`). The
  `outcome` entries an earlier crystal added as each task closed are still listed and found by a search, but
  agents starting aren't shown them; `crystal memory rm` those you don't want. Each keeps its task's goal in
  a sentence and what `crystal done` said: they held the whole brief the task was given, pages of it at times,
  which the task's record keeps.
- Lessons, `decision`, `gotcha` and `command`, rank above notes and outcomes: in a search and at launch, a
  note or an outcome ranks three places lower than what it says would put it, below the lessons near it but
  still ahead of one that hardly answers. A search about what was done, with a word like `did`, `done`,
  `merged`, `already` or `history` in it, ranks them where they fall.
- A note or an outcome nobody finds again expires: a note 30 days after it was said, an outcome after 14. Found
  again is said again (`remembered 3 already`), or read in full by an agent, with the `memory_show` tool or
  `crystal memory show` in a session. A search and agents starting leave the expired out; they're kept, marked
  `[expired]` in the list, `list --expired` lists them alone and `search --all` brings them back. Lessons never
  expire. The entries kept before crystal told whether one was found again have their days counted from when
  it started to, the upgrade, not from when they were said; so does an entry made a note.
- `kind` changes the kind of entries, by their ids, once every one is there: say a lesson kept as a note to a
  `gotcha`, which then ranks with the lessons and never expires. What it says stays as it is, and so do its
  vector and its place in the index. `kind --notes` has the distiller's model (`distill_model`, Haiku) read
  every note, 40 at a time, in one `claude -p` as locked down as the distiller's, and lists those it reads as
  lessons with the kind it gives each; `--yes` gives them it. Notes that read as status (`list --status`) are
  left out. It's a model's reading, so look through what it lists first; a pass costs a few cents.
- `list --status` lists the entries that read as progress or status rather than lessons, by the words that
  say so: merged, pushed, committed or installed, CI passing, a pull request opened, a commit's hash, a backlog
  item that tracks it, or what holds only in this pull request. It's for you to look through: `rm --status`
  lists what it would forget, and forgets them only with `--yes`; `-k` keeps either to a kind. Tasks' outcomes
  aren't counted: `-k outcome` lists those, and they expire.
- A project is its main worktree, so every worktree of it shares one list. Every project's list is kept in one
  SQLite database in crystal's state directory (`~/.local/state/crystal/memory/memory.db`), not in the
  repository. A project's list from before, a JSON file there, is brought in the first time it's read.
- `search` uses SQLite's full-text index (FTS5), ranked by bm25: any of the words matches, and so does a word
  they start or stem from (`deploying` finds "Deploys go out on Tuesdays"); the entries with more of the words,
  and rarer ones, come first, drifting ones marked where they rank, and of those it finds, the stale come
  after the rest, marked. `--fresh` leaves the stale out. `-k` keeps to a kind, `-f` to the entries about a
  file, or about any file in a directory, named from where you are (give it more than once for more), and `-n`
  says how many at most (50 unless it's told). With [search by meaning](#search-by-meaning), on unless you turn
  it off, entries that mean the same count too, whatever their words, and what doesn't answer the search is
  left out. Expired entries are left out too, and `--all` (`-a`) brings them back, marked.
- The same thing remembered again (the same words, whatever the case or punctuation) is the one entry, seen
  again: `remembered 3 already`. So is the same said in other words, by you, an agent or the distiller, once
  [search by meaning](#search-by-meaning) is on: `remembered 3 already, in other words: <what 3 says>`. Seen
  again, an entry counts it (`show` says how many times it was said), takes any new files it names, and
  holds again for its files and what it names as they are now. Credentials in an entry, like `API_KEY=…` or a token, are taken
  out as it's kept.
- `dedupe` finds the entries that were kept twice before that, in other words, and lists them, each group
  under the one it would keep, with how alike each is to it: one you or an agent remembered rather than one
  the distiller said, then the one most of the others say the same as, then the earliest. Nothing changes
  until `--apply`: then the one kept counts every time each of the others was said, so it doesn't expire,
  takes their files, and holds as well as the freshest of them (when one still holds, it's anchored to its
  files and what it names as they are now, as though said again). The others leave the list (`show` says where one went), kept
  apart so their words said again count as the one kept said again, and the distiller can't add them back.
  Each one goes straight into the one kept, never through another, so two that only both look like a third
  stay apart. It needs the models, and takes under a minute on a few hundred entries.
- Whether an entry still holds goes by what it names: the identifiers, paths, commands and flags in its text,
  in backticks or shaped like code (`local_origin`, `TaskRecord`, `Request::Shutdown` as `Shutdown`,
  `src/agent_rules.rs`, `agents/`, `--test-threads`), that are in the worktree's code as it's remembered:
  every word of every file git lists there, tracked or new and not ignored. While they're all still there, it
  holds, however much its files change. Once some are gone, it's marked drifting: it may hold only in part.
  Once all of them are, it's stale: agents starting aren't shown it, and a search gives it after the rest.
  What it names that isn't in the code as it's remembered, like something just removed, isn't looked for.
  The daemon keeps each worktree's words from one look to the next, and reads again only the files that
  changed since (by their time and size), so a search, an agent starting and `crystal memory` itself, which
  asks the daemon when one is running, don't read the whole worktree each time.
- `-f` names a file an entry is about, and can be given more than once. crystal keeps a hash of each file as it
  is then (a file that isn't there isn't counted). An entry that names nothing to look for goes by its files
  instead: drifting once some have changed, and stale once every one of them is gone. `crystal memory show`
  says what's gone. `crystal memory rm` an entry that no longer holds, or remember it again, which takes what
  it names and its files as they are now.
  The code is looked at in the worktree the entry was remembered in while that's there, and in the main
  worktree after.
- `promote` asks first at a terminal; `--yes` doesn't. It writes to CLAUDE.md, or to AGENTS.md when that's the
  only one the project has.
- `rm` forgets an entry: it leaves the list, and the distiller never adds it back. `memory list --forgotten`
  (or `--wrong`) lists what was forgotten, as it was; remembering it again brings it back.
- `m` in the sidebar opens the selected session's project's list, drifting, stale and expired entries marked:
  the entry the bar is on is shown in full beside it, with what's gone, `/` filters, `Enter` opens its file in
  your `$EDITOR` (in the worktree it was remembered in while that's there, as a session of its own), `x`
  forgets an entry and `p` promotes it, each after a `y`, and `c` gives it the kind the next key picks (`d`,
  `g`, `c` or `n`).

## What an agent is shown

When an agent starts, crystal shows it the entries that have most to do with its launch: first those about files
its worktree has changed since its branch left the default one (`origin`'s, or `main` or `master`), committed
or not, then those that have most to do with its first prompt, or the newest when neither finds any, lessons
before the notes near them. That's a few at most, in 800 bytes, the least relevant left out first; none that's
stale, expired or a task's outcome, and drifting ones marked where they rank. Each comes with its id, and a
line on how to read the rest and add more:

- Claude Code gets them in its system prompt, and reads the rest with crystal's MCP tools (below).
- Codex gets them as its `developer_instructions` (`-c`), after the ones it has already, from a
  [profile](configuration.md#profiles) or its own `config.toml`, and reads the rest with `crystal memory search` and `show`.
- Gemini CLI, OpenCode, Cursor, Qwen Code and Pi get them at the top of their first prompt, when they're given
  one. A prompt that would pass 16 KiB with them loses what the memory has first. An agent that takes no first
  prompt, like Aider, isn't told.

`crystal plugin disable memory` turns it all off: see [plugins](plugins.md).

## Through MCP

Every Claude Code session crystal starts, in a terminal or as a task in the background (`claude -p`), gets
crystal's own MCP server, `crystal mcp`, with its two tools allowed: `memory_search`, which searches the
project's memory the way `crystal memory search` does, and `memory_show`, which reads one entry in full, and
finds it again, so it doesn't expire. These
are how it reads the rest of what was learned without a shell command, which a task has nobody to say yes to
and a session in a terminal would stop to ask about.

## Search by meaning

Words only find words: "db" never finds "Postgres has to be running". So crystal also searches by what entries
mean, with two models run on your machine through [Candle](https://github.com/huggingface/candle): no API, no
key, and nothing leaves the machine.

- [jinaai/jina-embeddings-v5-text-small](https://huggingface.co/jinaai/jina-embeddings-v5-text-small) turns
  each entry, and each query, into a vector, so an entry that means what a query asks is found whatever its
  words.
- [jinaai/jina-reranker-v3](https://huggingface.co/jinaai/jina-reranker-v3) then reads the query with the 20
  entries found best, by words and meaning together, and scores how well each answers it: what answers comes
  first, what doesn't is left out, and a search about something the memory doesn't hold finds nothing, rather
  than whatever is nearest.

Asked 97 questions about crystal's own memory, it had the right entry among the first five for 92% of them,
against 75% by words alone, and found nothing for most questions the memory couldn't answer.

```sh
crystal memory embed   # downloads both models now (2.4 GB), and gives every entry its vector
```

- The models aren't part of crystal. The daemon downloads them in the background as it starts, when they
  aren't there yet, or `crystal memory embed` does now: with `curl`, at pinned revisions, each file checked
  against its SHA-256, kept in `~/.cache/crystal/models/` (or `$XDG_CACHE_HOME`). Until they're there,
  searches go by words alone, and `crystal memory search` says so.
- They run on a Mac's GPU (Metal), or on the CPU elsewhere; `CRYSTAL_MODELS_ON_CPU=1` keeps them on the CPU
  on a Mac too. The daemon keeps them loaded, about 2.5 GB, once for every client: `crystal memory search`
  and every task's `memory_search` ask it, and only search in their own process when no daemon is running. A
  search takes about half a second on an Apple silicon Mac, most of it the reranker's; on a CPU, a few
  seconds.
- Each entry's vector is kept beside it in `memory.db`. An entry without one, say one remembered while the
  models were off, gets it the first time a search needs it, and vectors from a model crystal no longer uses
  are let go.
- An entry being added is held against those there already, tasks' outcomes aside: one whose vector is as
  alike as 0.92 says the same thing; one as alike as 0.87 does when the reranker, reading the new one as the
  query, scores it 0.40 or more. On crystal's own memory, every pair that alike said the same thing, and
  different lessons about the same thing were alike up to 0.90 but the reranker scored them 0.36 at most;
  the same lesson said again, mostly from 0.4 to 0.75. Some rewordings slip through as their own entries:
  keeping one twice does less harm than merging two that differ. Without the models, only the same words
  count.
- A search ranks by words (bm25) and by meaning, and merges the two by reciprocal rank fusion, so an entry
  high in both comes first; by meaning, only the entries close to the best match count. Then the reranker
  reads the first 20: when not even the best answers the query, the search finds nothing; otherwise the ones
  it rules out are left out, and its ranking is merged in too. What a session is shown as it starts, and what
  the distiller is shown the memory has already, go the same way.
- Both models are licensed [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/): yours to use,
  but not commercially. `embeddings = false` under `[memory]` turns search by meaning off, and `rerank =
  false` leaves the reranker out: faster on a CPU, but a search then always brings back what's nearest.

## The distiller

Agents don't always remember what they learned. So once a task closes, done or failed, a model reads what it
did and keeps what a later session would need to know and couldn't find in the code: decisions and why, dead
ends, commands that work, traps. So it does once a session is archived (`A`, or `crystal archive`), unless its
task closed done or failed and it was read then. It's one `claude -p` run on Haiku, in the background:

- It reads the end of what the task did: a task's transcript, as Claude Code keeps it (or, when it doesn't,
  what crystal read of its runs), or for Claude Code in a terminal, the transcript its hooks named. Codex leaves
  nothing it can read. Credentials are taken out before the model sees any of it.
- It's shown what the project's memory has already on the same subject, each entry by its id and kind, and
  told never to give that again: with search by meaning on, the entries nearest in meaning to the last things
  the session said, where what it keeps comes from, merged with those that have most to do with its task.
  Of the notes among them, those it says are lessons it makes `decision`, `gotcha` or `command` (a note that
  reads as status it's never asked about), and `memory.changed` says so.
- It's told to keep lessons alone, what a later session couldn't get from the code, the git log, the backlog or
  CLAUDE.md, and never progress or status (merged, pushed, installed, CI passed), a commit's hash or a pull
  request's or backlog item's number as the point of an entry, or what's only true today, with entries of each
  from crystal's own memory; never to say what you decided unless the transcript shows you saying it; to
  quote names exactly, with no line numbers; and to make an entry a note only when it's no lesson.
- It has no tools, no MCP servers, none of the project's settings and none of your hooks, a budget (25 cents
  by default) and two turns. Its answer is checked before anything is kept: at most 8 entries, of the kinds
  `decision`, `gotcha`, `command` and `note`, each 400 characters at most, naming only files that are in the
  checkout.
- What passes is kept like anything else, `from the distiller, after task <name>`: what's there already, in
  its words or others, is seen again rather than added twice, and what you forgot with `rm` it never adds
  back (you can, by remembering it yourself).
- It's also shown up to 4 of the stale entries about the files the work touched (those its branch changed
  since it left the default one, and those it edited), with what each names that's gone, and says of each the
  record settles whether it still holds (it's anchored again, to the code as it is), holds once reworded (its
  text is replaced, under the same id), or no longer holds (it's forgotten, as `rm` forgets). A verdict on an
  entry it wasn't shown is refused, and so is one that keeps an entry, or rewords it, naming only what isn't
  in the checkout.
- How it went is a line in the daemon's log, `default.log` beside the socket. `crystal memory distill <session>`
  runs it now and says what came of it; it works on any Claude Code session or task, closed or not.

`[memory]` in the [settings](configuration.md) changes how it runs:

```toml
[memory]
distill = true                      # false to turn it off, as tasks close and sessions are archived
distill_model = "claude-haiku-4-5"  # the model, as `claude --model` takes it
distill_budget_usd = 0.25           # the most one task's pass may spend
embeddings = true                   # false to search by words alone: see above
rerank = true                       # false to leave the reranker out
```
