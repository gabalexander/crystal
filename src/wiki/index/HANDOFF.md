# wiki-index handoff (branch feat/wiki-index, WIP commit)

The symbol index the wiki's `code:` links come from. Code is in `src/wiki/index/`
of crystal's `feat/wiki-index` branch (rebased on master 11b2542, which has wiki-serve #124).

## Interface (as the brief specified, unchanged)
`src/wiki/index/mod.rs`: `Def {name, qualified, kind, path, start, end, precise}`, `DefKind`
(module, struct, class, enum, union, trait, interface, type, function, method, field, variant,
const, static, variable, macro, command, flag, file, directory), `Lookup {Unique, Ambiguous, Missing}`,
`Index::build(root, commit, cache, &IndexSettings)`, `lookup(span, near)`, `defined_at(path, line)`,
`outline(files)`, plus `report()` / `summary()` (per-language tier and why), `Def::target()` ->
`src/x.rs#L10-L20` / `#L10` / whole file. File/dir/file-module defs have start=end=0.
Settings: `IndexSettings` = `[wiki.index]` in crystal's config (precise, indexer_timeout_secs=900,
indexer_memory_mb=8192, max_file_kb=1024, exclude=[vendor, third_party, node_modules, testdata]).

## Files
- `files.rs`: `git ls-tree -r -z -l <commit>` (blobs, sizes; no submodules/symlinks), `git cat-file --batch`
  streaming, `Lang::of` by extension (.h -> C++ grammar; .ts/.js both via TSX grammar), test files by path,
  module/package prefix per language (Rust from path under last `src/`, Python dotted, Go/Java package clause),
  program names from Cargo.toml/package.json/pyproject + `cmd/<name>/`.
- `grammar.rs`: tree-sitter walker with per-language rules (Rust, Go, Python, TS/JS, Java, C/C++). Not the
  upstream tags queries (they lack fields/consts/variants/containers). Reads Rust attributes: serde rename /
  rename_all / alias -> config key names; clap `#[arg(long, short)]` -> `--flag`/`-f`; Subcommand variants ->
  kebab words / `#[command(name)]`; Go struct tags; Go cobra `Use:` and flag-set calls; Python
  add_argument/click option; commander option/command. Doesn't descend into function bodies.
- `keywords.rs`: regex-per-line tier for languages without a compiled grammar (Kotlin, C#, Swift, Ruby, PHP,
  shell, Scala, Lua, Elixir, Dart, Zig, Perl, Groovy); end lines by indentation; nesting by indentation.
- `scip.rs`: own protobuf wire reader of SCIP (streams Index field by field, a Document at a time), symbol
  descriptor parser, rust-analyzer `impl#[Type][Trait]method().` -> `Type::method`, Go package path -> last part.
- `precise.rs`: runs installed indexers (rust-analyzer via `rustup which` then PATH, verified with --version;
  scip-go, scip-typescript, scip-python, scip-java, scip-clang) on outermost manifest dirs (<=4 per indexer),
  in the repo itself if HEAD==commit (files changed vs commit dropped from precise), else `git archive` copy
  in cache. Process group, killed past timeout or memory (ps pgid rss sum). Cache per project keyed by
  sha256 of indexer+version+files' blobs+manifests+locks. Failure -> grammar tier + note with log path.
- `cache.rs`: `syntax.json` by `<blob>:<Lang>`, parse missing on <=8 threads, rewritten each build.
- `lookup.rs`: span -> queries (path / command line / flag / `[table] key` / dotted key / name) tried in order;
  narrowing: kind hint (`()`, `!`, `struct X`) -> near files -> not test -> definition over declaration ->
  the one SCIP says near files refer to -> else Ambiguous. Config keys and subcommands are followed through
  field/variant types (`[sessions] stop_idle_after` -> field whose container is the type of field `sessions`).
- `cli.rs`: `crystal wiki index [-C DIR] [--commit REV] [--near PATH]... [--json] [-- SPAN...]` (in main.rs).
- `tests.rs`, `grammar/tests.rs`: 30 unit tests, all pass (`cargo test --bin crystal wiki::index`).

## Decisions
- Grammars compiled in: Rust, Go, Python, TS/TSX(+JS via TSX), Java, C, C++ ~8.1 MB (+0.83 MB gzipped);
  the other 6 asked-for (C#, Kotlin, Swift, Ruby, PHP, bash) would add ~17 MB (all 13 = 22 MB, +2.3 MB gz,
  vs crystal's 27 MB / 9.6 MB gz) -> they go to the keywords tier. A cargo feature `all-grammars` was
  started and removed for the WIP because their walker rules aren't written (tree dumps in samples/ help).
- No downloading of indexers: each needs its toolchain anyway (rust-analyzer: cargo + build scripts;
  scip-go: go), rust-analyzer runs the project's build scripts/proc macros (don't run a fetched binary on
  that), and the report prints the one-line install. Release digests for pinning are in the GitHub API
  (`assets[].digest`) if that's revisited: rust-analyzer 2026-10-05, scip-go v0.2.7 (now scip-code/scip-go,
  no darwin-amd64 asset).
- tree-sitter 0.26.13 (0.27 needs rustc 1.90; crystal's MSRV is 1.88). Swift 0.7.3 and PHP 0.24.2 picked
  over versions < 2 weeks old.

## Measurements (macOS arm64)
- Grammar size added to a release binary (KB): rust 1178, go 290, python 532, ts+tsx 2888 (tsx alone 1500),
  javascript 484, java 484, kotlin(-ng) 3468, c 694, cpp 3454, c-sharp 5322, ruby 2147, swift 3762, php 1114,
  bash 1425, scala 3971, lua 129. opt-level=s doesn't help (data tables). musl not measured (needs CI:
  `gh workflow run release.yml` builds all 4 targets without publishing).
- crystal (Rust, 299 files at master): syntactic 15.7k defs, 3.9 s debug build cold, cache 1.6 MB.
  `rust-analyzer scip` on crystal: 39 s, 2.1 GB RSS (3.6 GB peak footprint), 35 MB .scip, 181 MB target/
  for build scripts. Gives enclosing ranges and kinds.
- podman (10,130 files, 1,422 own Go files): syntactic 18.0k defs, 1.2 s debug cold, 0.3 s cached, 74 MB RSS,
  cache 1.9 MB. With scip-go 0.2.7: 24 s, 683 MB RSS, 1,045/1,420 Go files precise (host GOOS=darwin build
  tags drop linux-only files; consider GOOS=linux or a setting), 19.6k defs, precise cache 7.8 MB.
- No TS/Python repo measured yet; coverage/precision on a real wiki.json not measured yet (wiki-gen's wasn't
  there). No regex-scan comparison yet.

## Lookup behaviour seen (crystal at 22ee18c; podman)
Good: `Session::stop`, `[sessions] stop_idle_after`, `stop_idle_after`, `crystal send --wait` (-> the field),
`crystal restart-server`, `src/daemon.rs`, `src/tui/`, `daemon.rs`, `outln!`, `tui::app::App`,
`src/daemon.rs:120`, `entities.PodmanConfig`, `PodmanConfig.FlagSet`. Correctly unlinked: `new`, `--wait`
alone (4 commands), `Vec<Session>`, `cargo test`.
To tune: bare `Session` (11 candidates: struct + variants/fields named Session) and `PodmanConfig`
(struct + func) -> add "a capitalized bare name prefers the one type-kind def" and measure;
`[[profile]]` -> prefer a field whose serde rename equals the key / top-level config struct;
`podman build` -> two cobra `build` commands (farm, images) need parent-command following;
`--squash` resolved to commit.go's flag but `podman build --squash` exists too (defined another way) ->
flags from Go may be false-unique; consider leaving Go flags unlinked unless near.

## Left to do
- Precision/coverage measurement on wiki-web's fixture spans and wiki-gen's real wiki.json (automated check:
  target lines contain the name, kind matches; spot-check 50), regex-scan comparison, a TS or Python repo.
- The tie-break tunings above; Go GOOS for scip-go.
- Walker rules for C#, Kotlin, Swift, Ruby, PHP, bash if grammars are compiled in (feature).
- Docs (docs/wiki.md section "The index", AGENTS.md layout entries), tests/cli.rs e2e for `crystal wiki index`.
- A tiny .scip fixture test exists (scip::tests writes one with `scip::write`); a test that runs a fake
  indexer script end to end through `precise::run` would be good.

## Scratchpad (this dir)
- `samples/`: sample sources per language; `tssize/target/release/tssize <lang> <file>` dumps a tree-sitter tree
  (langs rust go python tsx java kotlin c cpp csharp ruby swift php bash) - use it to write walker rules.
- `tssize/`: the size-measurement crate (features per grammar).
- `tools/rust-analyzer` (2026-10-05 standalone), `tools/scip-go` (0.2.7): put on PATH to get the precise tier.
- `crystal.scip` (rust-analyzer on crystal 22ee18c), `podman.scip` (scip-go on podman ac91395).
- `py/scipdump.py <file.scip> [path]`: dumps documents/definitions/symbols of a .scip.
- `dl/proto/scip.proto`: the schema.
- `artifacts/`: `crystal-syntax.json`, `podman-syntax.json` (syntactic caches), `podman-scip-go-kept.json`.
- `repos/crystal`, `repos/podman`: `git archive` exports used for the indexer runs.
- Google Code Wiki reference screenshots: /private/tmp/claude-501/-Users-gabrielpop-personal-crystal/64cd3a98-3cf3-49f8-9690-828b06e3c55f/scratchpad/codewiki-ref/
