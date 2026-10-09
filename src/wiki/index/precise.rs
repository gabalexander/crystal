//! The precise tier: the SCIP indexers installed here, each run on the
//! projects of the repository in its languages (a directory with a
//! `Cargo.toml`, a `go.mod`, a `tsconfig.json`, ...), the outermost ones,
//! and what they write read ([`super::scip`]) into each file's definitions
//! and the definitions each file refers to.
//!
//! An indexer runs where the repository's files are at the wiki's commit:
//! in the repository itself when it's checked out there, what's changed in
//! it since left to the grammars, or else in a copy of the commit that `git
//! archive` writes into the cache. It's held to `[wiki.index]`'s time and
//! memory, every process under it counted, and stopped past either; what
//! it said is kept in a log beside its cache. What it read is kept by a
//! hash of its files' blobs, so it runs again only once one of them
//! changes.
//!
//! None is downloaded: each needs its language's toolchain, and a user who
//! has that has its indexer one command away, which the report says; and
//! an indexer runs the project's build scripts, which crystal shouldn't
//! start with a program it fetched.

use super::DefKind;
use super::IndexSettings;
use super::files::{self, Entry, Lang};
use super::scip;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// What the indexers said of the repository.
#[derive(Debug, Default)]
pub struct Run {
    /// Each file an indexer covered, by its path from the top.
    pub docs: HashMap<String, Doc>,
    /// For each language with an indexer, the one that ran and how long it
    /// took, or why none did.
    pub notes: HashMap<Lang, String>,
}

/// What an indexer said of a file.
#[derive(Debug, Default, Clone)]
pub struct Doc {
    pub defs: Vec<Def>,
    /// The definitions it refers to, by their ids.
    pub refs: Vec<u32>,
}

/// A definition an indexer found, with an id the run gives its symbol.
#[derive(Debug, Clone)]
pub struct Def {
    pub id: u32,
    pub name: String,
    pub segments: Vec<String>,
    pub kind: DefKind,
    pub line: u32,
    pub start: u32,
    pub end: u32,
    pub test: bool,
}

/// An indexer crystal knows how to run.
struct Indexer {
    program: &'static str,
    langs: &'static [Lang],
    /// The files that make a directory a project it indexes, the one it
    /// prefers first.
    manifests: &'static [&'static str],
    /// How to install it.
    install: &'static str,
}

const INDEXERS: &[Indexer] = &[
    Indexer {
        program: "rust-analyzer",
        langs: &[Lang::Rust],
        manifests: &["Cargo.toml"],
        install: "rustup component add rust-analyzer",
    },
    Indexer {
        program: "scip-go",
        langs: &[Lang::Go],
        manifests: &["go.mod"],
        install: "go install github.com/scip-code/scip-go/cmd/scip-go@latest",
    },
    Indexer {
        program: "scip-typescript",
        langs: &[Lang::TypeScript, Lang::JavaScript],
        manifests: &["tsconfig.json", "package.json"],
        install: "npm install -g @sourcegraph/scip-typescript",
    },
    Indexer {
        program: "scip-python",
        langs: &[Lang::Python],
        manifests: &["pyproject.toml", "setup.py", "setup.cfg"],
        install: "npm install -g @sourcegraph/scip-python",
    },
    Indexer {
        program: "scip-java",
        langs: &[Lang::Java, Lang::Kotlin, Lang::Scala],
        manifests: &["pom.xml", "build.gradle", "build.gradle.kts", "build.sbt"],
        install: "cs install --contrib scip-java",
    },
    Indexer {
        program: "scip-clang",
        langs: &[Lang::C, Lang::Cpp],
        manifests: &["compile_commands.json"],
        install: "scip-clang from github.com/sourcegraph/scip-clang/releases, and the build's compile_commands.json",
    },
];

/// Lock files, whose change can change what an indexer sees.
const LOCKS: &[&str] = &[
    "Cargo.lock",
    "go.sum",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "uv.lock",
];

/// The most projects an indexer is run on in one repository.
const MOST_PROJECTS: usize = 4;

/// A directory an indexer runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Project {
    /// From the top of the repository, empty for the top itself.
    dir: String,
    /// The manifest that makes it one, from the top.
    manifest: String,
}

/// What the cache keeps of an indexer's run on a project.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Kept {
    version: u32,
    key: String,
    tool: String,
    docs: Vec<KeptDoc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeptDoc {
    path: String,
    defs: Vec<KeptDef>,
    /// The symbols it refers to that the project defines.
    refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeptDef {
    symbol: String,
    segments: Vec<String>,
    kind: DefKind,
    line: u32,
    start: u32,
    end: u32,
    #[serde(default)]
    test: bool,
}

/// The version of what the cache keeps; another is read again.
const KEPT: u32 = 1;

/// Runs every indexer installed here whose languages are among `read`, the
/// files read for definitions, on the projects among `entries`, the
/// repository's files at `commit`, keeping what each said in `cache`.
pub fn run(
    root: &Path,
    commit: &str,
    cache: &Path,
    settings: &IndexSettings,
    entries: &[Entry],
    read: &[&Entry],
) -> Run {
    let mut run = Run::default();
    let present: HashSet<Lang> = read.iter().filter_map(|entry| entry.lang).collect();
    let dir = cache.join("precise");
    let _ = fs::create_dir_all(&dir);
    let mut checkout: Option<Result<Checkout, String>> = None;
    let mut docs: Vec<KeptDoc> = Vec::new();
    for indexer in INDEXERS {
        let langs: Vec<Lang> = (indexer.langs.iter().copied())
            .filter(|lang| present.contains(lang))
            .collect();
        if langs.is_empty() {
            continue;
        }
        let mut say = |note: String| {
            for lang in &langs {
                run.notes.insert(*lang, note.clone());
            }
        };
        let projects = projects(root, indexer, entries, read, settings);
        if projects.is_empty() {
            say(format!(
                "no {} for {} to start from",
                indexer.manifests.join(" or "),
                indexer.program
            ));
            continue;
        }
        let Some((program, version)) = locate(indexer.program) else {
            say(format!(
                "{} isn't installed ({})",
                indexer.program, indexer.install
            ));
            continue;
        };
        let started = Instant::now();
        let mut ran = Vec::new();
        let mut failed = None;
        for (at, project) in projects.iter().enumerate() {
            let key = key(indexer, &version, project, entries);
            let kept_at = dir.join(format!(
                "{}-{}.json",
                indexer.program,
                short_hash(&project.dir)
            ));
            if let Some(kept) = load(&kept_at).filter(|kept| kept.key == key) {
                ran.push(kept);
                continue;
            }
            let here = checkout.get_or_insert_with(|| Checkout::make(root, commit, cache));
            let here = match here {
                Ok(here) => &*here,
                Err(err) => {
                    failed = Some(err.clone());
                    break;
                }
            };
            let output = dir.join(format!("{}-{at}.scip", indexer.program));
            let log = dir.join(format!("{}.log", indexer.program));
            let read =
                index(indexer, &program, here, project, &output, &log, settings).and_then(|()| {
                    let file =
                        File::open(&output).map_err(|err| format!("wrote no index ({err})"))?;
                    scip::read(file)
                        .map_err(|err| format!("wrote an index crystal can't read ({err:#})"))
                });
            let _ = fs::remove_file(&output);
            match read {
                Ok(read) => {
                    let kept = Kept {
                        version: KEPT,
                        key,
                        tool: match read.tool.is_empty() {
                            true => format!("{} {version}", indexer.program),
                            false => read.tool.clone(),
                        },
                        docs: keep(&read, project, here),
                    };
                    let _ = save(&kept_at, &kept);
                    ran.push(kept);
                }
                Err(err) => {
                    failed = Some(format!(
                        "{} {err}, so the grammar read it instead; its log is {}",
                        indexer.program,
                        log.display()
                    ));
                    break;
                }
            }
        }
        match failed {
            Some(note) => say(note),
            None => {
                let tool = ran
                    .first()
                    .map(|kept| kept.tool.clone())
                    .unwrap_or_default();
                let projects = match ran.len() {
                    1 => String::new(),
                    count => format!(", {count} projects"),
                };
                say(format!(
                    "by {tool}{projects} in {}s",
                    started.elapsed().as_secs()
                ));
                docs.extend(ran.into_iter().flat_map(|kept| kept.docs));
            }
        }
    }
    if let Some(Ok(Checkout::Copy(copy))) = &checkout {
        let _ = fs::remove_dir_all(copy);
    }
    let paths: HashSet<&str> = read.iter().map(|entry| entry.path.as_str()).collect();
    docs.retain(|doc| paths.contains(doc.path.as_str()));
    run.docs = ids(docs);
    run
}

/// The projects among `entries` that `indexer` runs on: the outermost
/// directories with one of its manifests and a file of its languages to
/// read, a few at most. scip-clang's compilation database is the build's,
/// so it's looked for in the checkout.
fn projects(
    root: &Path,
    indexer: &Indexer,
    entries: &[Entry],
    read: &[&Entry],
    settings: &IndexSettings,
) -> Vec<Project> {
    if indexer.program == "scip-clang" {
        let database = ["compile_commands.json", "build/compile_commands.json"];
        let found = database.iter().find(|path| root.join(path).is_file());
        return found
            .map(|path| Project {
                dir: String::new(),
                manifest: path.to_string(),
            })
            .into_iter()
            .collect();
    }
    let mut candidates: Vec<Project> = Vec::new();
    for entry in entries {
        let (dir, name) = match entry.path.rsplit_once('/') {
            Some((dir, name)) => (dir, name),
            None => ("", entry.path.as_str()),
        };
        let Some(rank) = indexer
            .manifests
            .iter()
            .position(|manifest| *manifest == name)
        else {
            continue;
        };
        if files::excluded(&entry.path, &settings.exclude) {
            continue;
        }
        match candidates.iter_mut().find(|project| project.dir == dir) {
            Some(project) => {
                let had = project.manifest.rsplit('/').next().unwrap_or("");
                if indexer
                    .manifests
                    .iter()
                    .position(|manifest| *manifest == had)
                    > Some(rank)
                {
                    project.manifest = entry.path.clone();
                }
            }
            None => candidates.push(Project {
                dir: dir.to_string(),
                manifest: entry.path.clone(),
            }),
        }
    }
    candidates.sort_by_key(|project| {
        (
            project.dir.matches('/').count() + usize::from(!project.dir.is_empty()),
            project.dir.clone(),
        )
    });
    let mut projects: Vec<Project> = Vec::new();
    for project in candidates {
        if projects.iter().any(|outer| under(&project.dir, &outer.dir)) {
            continue;
        }
        let has_code = read.iter().any(|entry| {
            entry.lang.is_some_and(|lang| indexer.langs.contains(&lang))
                && under(&entry.path, &project.dir)
        });
        if has_code {
            projects.push(project);
        }
        if projects.len() == MOST_PROJECTS {
            break;
        }
    }
    projects
}

/// Whether `path` is in the directory `dir`, the top being empty.
fn under(path: &str, dir: &str) -> bool {
    dir.is_empty() || path == dir || path.starts_with(dir) && path[dir.len()..].starts_with('/')
}

/// What `indexer`'s run on `project` depends on: the indexer, and the blob
/// of each of the project's files in its languages, its manifests and its
/// lock files.
fn key(indexer: &Indexer, version: &str, project: &Project, entries: &[Entry]) -> String {
    let mut hasher = Sha256::new();
    for part in [indexer.program, version, &project.dir, &project.manifest] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    hasher.update(KEPT.to_le_bytes());
    for entry in entries {
        let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
        let counts = entry.lang.is_some_and(|lang| indexer.langs.contains(&lang))
            || indexer.manifests.contains(&name)
            || LOCKS.contains(&name);
        if counts && under(&entry.path, &project.dir) {
            hasher.update(entry.path.as_bytes());
            hasher.update([0]);
            hasher.update(entry.blob.as_bytes());
        }
    }
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A short hash of `text`, for a file name.
fn short_hash(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))[..12].to_string()
}

fn load(path: &Path) -> Option<Kept> {
    let kept: Kept = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    Some(kept).filter(|kept| kept.version == KEPT)
}

/// Writes `kept` to `path`, beside it first so a reader never finds half.
fn save(path: &Path, kept: &Kept) -> std::io::Result<()> {
    let partial = path.with_extension("json.part");
    fs::write(&partial, serde_json::to_vec(kept)?)?;
    fs::rename(&partial, path)
}

/// Where an indexer runs: the repository itself, checked out at the commit
/// but for the files in `changed`, or a copy of the commit.
#[derive(Debug)]
enum Checkout {
    Here {
        root: PathBuf,
        changed: HashSet<String>,
    },
    Copy(PathBuf),
}

impl Checkout {
    /// The repository at `root` if it's checked out at `commit`, or else a
    /// copy of `commit` in `cache`.
    fn make(root: &Path, commit: &str, cache: &Path) -> Result<Checkout, String> {
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| output.stdout)
        };
        let head = git(&["rev-parse", "HEAD"])
            .map(|head| String::from_utf8_lossy(&head).trim().to_string());
        if head.as_deref() == Some(commit) {
            let changed =
                git(&["diff", "--name-only", "--no-renames", "-z", commit]).unwrap_or_default();
            let changed = changed
                .split(|&byte| byte == 0)
                .filter(|path| !path.is_empty());
            return Ok(Checkout::Here {
                root: root.to_path_buf(),
                changed: changed
                    .map(|path| String::from_utf8_lossy(path).into_owned())
                    .collect(),
            });
        }
        let copy = cache.join("checkout");
        let _ = fs::remove_dir_all(&copy);
        fs::create_dir_all(&copy)
            .map_err(|err| format!("couldn't make {}: {err}", copy.display()))?;
        let mut archive = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["archive", "--format=tar", commit])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("couldn't run git archive: {err}"))?;
        let tar = Command::new("tar")
            .arg("-x")
            .arg("-C")
            .arg(&copy)
            .stdin(archive.stdout.take().map_or_else(Stdio::null, Stdio::from))
            .stderr(Stdio::null())
            .status();
        let archived = archive.wait().is_ok_and(|status| status.success());
        if !archived || !tar.is_ok_and(|status| status.success()) {
            let _ = fs::remove_dir_all(&copy);
            return Err(format!(
                "couldn't copy the files at {commit} for the indexers"
            ));
        }
        Ok(Checkout::Copy(copy))
    }

    fn dir(&self) -> &Path {
        match self {
            Checkout::Here { root, .. } => root,
            Checkout::Copy(copy) => copy,
        }
    }

    /// Whether the file at `path` isn't as it is at the commit.
    fn changed(&self, path: &str) -> bool {
        match self {
            Checkout::Here { changed, .. } => changed.contains(path),
            Checkout::Copy(_) => false,
        }
    }
}

/// Runs `indexer` on `project`, writing its index to `output` and what it
/// says to `log`, held to the settings' time and memory.
fn index(
    indexer: &Indexer,
    program: &Path,
    checkout: &Checkout,
    project: &Project,
    output: &Path,
    log: &Path,
    settings: &IndexSettings,
) -> Result<(), String> {
    let dir = checkout.dir().join(&project.dir);
    let mut command = Command::new(program);
    match indexer.program {
        "rust-analyzer" => {
            command.args(["scip", "."]).arg("--output").arg(output);
        }
        "scip-go" => {
            command.args(["index", "--output"]).arg(output);
        }
        "scip-typescript" => {
            command.args(["index", "--output"]).arg(output);
            if project.manifest.ends_with("package.json") {
                command.arg("--infer-tsconfig");
            }
        }
        "scip-python" => {
            let name = Path::new(&project.dir)
                .file_name()
                .or(checkout.dir().file_name());
            let name = name.map_or_else(
                || "project".into(),
                |name| name.to_string_lossy().into_owned(),
            );
            command
                .args(["index", ".", "--project-name", &name, "--output"])
                .arg(output);
        }
        "scip-java" => {
            command.args(["index", "--output"]).arg(output);
        }
        _ => {
            command
                .arg(format!("--compdb-path={}", project.manifest))
                .arg(format!("--index-output-path={}", output.display()));
        }
    }
    command.current_dir(&dir);
    bounded(
        command,
        log,
        Duration::from_secs(settings.indexer_timeout_secs),
        settings.indexer_memory_mb.saturating_mul(1024 * 1024),
    )
}

/// Runs `command`, what it says going to `log`, until it's done, stopping
/// it and every process under it once it has run longer than `limit` or
/// takes more than `memory` bytes.
fn bounded(mut command: Command, log: &Path, limit: Duration, memory: u64) -> Result<(), String> {
    let out =
        File::create(log).map_err(|err| format!("couldn't write {}: {err}", log.display()))?;
    let err = out.try_clone().map_err(|err| err.to_string())?;
    command
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0);
    let mut child = command
        .spawn()
        .map_err(|err| format!("couldn't start ({err})"))?;
    let pid = child.id();
    let started = Instant::now();
    let mut looked = Instant::now();
    let stop = |child: &mut std::process::Child| {
        // SAFETY: kill only sends a signal, to the group the child leads.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
        let _ = child.wait();
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("failed ({status}): {}", last_line(log))),
            Ok(None) => {}
            Err(err) => return Err(format!("couldn't be waited for ({err})")),
        }
        if started.elapsed() > limit {
            stop(&mut child);
            return Err(format!("took longer than {}s", limit.as_secs()));
        }
        if looked.elapsed() >= Duration::from_secs(1) {
            looked = Instant::now();
            if group_bytes(pid).is_some_and(|bytes| bytes > memory) {
                stop(&mut child);
                return Err(format!("took more than {} MB", memory / (1024 * 1024)));
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// What the processes in the group `group` leads take in memory, their
/// resident sets in bytes, as `ps` says, or `None` when it can't.
fn group_bytes(group: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-A", "-o", "pgid=,rss="])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    Some(group_bytes_in(
        &String::from_utf8_lossy(&output.stdout),
        group,
    ))
}

/// The memory the processes in the group `group` take, from `ps`'s
/// `pgid=,rss=` lines, each resident set in kilobytes.
fn group_bytes_in(listed: &str, group: u32) -> u64 {
    let group = group.to_string();
    listed
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next()? == group).then_some(())?;
            words.next()?.parse::<u64>().ok()
        })
        .sum::<u64>()
        * 1024
}

/// Text another program wrote, its control characters taken out, cut to
/// `most` characters.
fn printable(text: &str, most: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(most)
        .collect()
}

/// The last line in the log at `path` that says something.
fn last_line(path: &Path) -> String {
    let text = fs::read(path).unwrap_or_default();
    let tail = &text[text.len().saturating_sub(4096)..];
    let tail = String::from_utf8_lossy(tail);
    let line = tail
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("it said nothing");
    printable(line, 200)
}

/// Where `program` is, and its version: rust-analyzer as rustup has it,
/// else the first on `PATH` that answers `--version`, which the rustup proxy
/// for a component that isn't installed doesn't.
fn locate(program: &str) -> Option<(PathBuf, String)> {
    let mut candidates = Vec::new();
    if program == "rust-analyzer"
        && let Ok(output) = Command::new("rustup")
            .args(["which", "rust-analyzer"])
            .stderr(Stdio::null())
            .output()
        && output.status.success()
    {
        candidates.push(PathBuf::from(
            String::from_utf8_lossy(&output.stdout).trim(),
        ));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    candidates.extend(
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .filter(|path| path.is_file()),
    );
    candidates
        .into_iter()
        .find_map(|path| version_of(&path).map(|version| (path, version)))
}

/// What `program --version` says, within a few seconds, or `None` when it
/// fails.
fn version_of(program: &Path) -> Option<String> {
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait().ok()? {
            Some(status) => break status,
            None if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let text = reader.join().ok()?;
    let version = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    status.success().then(|| printable(version, 80))
}

/// What the cache keeps of what an indexer said of `project`: each file's
/// definitions, named as crystal names them, and its references to them,
/// leaving out the files that changed since the commit.
fn keep(read: &scip::Indexed, project: &Project, checkout: &Checkout) -> Vec<KeptDoc> {
    let defined: HashSet<&str> = (read.documents.iter())
        .flat_map(|document| &document.definitions)
        .map(|definition| definition.symbol.as_str())
        .collect();
    let mut docs = Vec::new();
    for document in &read.documents {
        let relative = document.path.trim_start_matches("./");
        let path = match project.dir.is_empty() {
            true => relative.to_string(),
            false => format!("{}/{relative}", project.dir),
        };
        if checkout.changed(&path) {
            continue;
        }
        let defs = document.definitions.iter().filter_map(|definition| {
            let (segments, suffix) = scip::segments(&definition.symbol)?;
            let kind = definition.kind.or_else(|| scip::kind_of_suffix(suffix))?;
            Some(KeptDef {
                symbol: definition.symbol.clone(),
                segments,
                kind,
                line: definition.line,
                start: definition.start,
                end: definition.end,
                test: definition.test,
            })
        });
        let refs = (document.references.iter())
            .filter(|symbol| defined.contains(symbol.as_str()))
            .cloned();
        docs.push(KeptDoc {
            path,
            defs: defs.collect(),
            refs: refs.collect(),
        });
    }
    docs
}

/// The docs by their paths, each symbol defined given an id.
fn ids(docs: Vec<KeptDoc>) -> HashMap<String, Doc> {
    let mut ids: HashMap<&str, u32> = HashMap::new();
    for def in docs.iter().flat_map(|doc| &doc.defs) {
        let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        ids.entry(def.symbol.as_str()).or_insert(next);
    }
    docs.iter()
        .map(|doc| {
            let defs = doc.defs.iter().map(|def| Def {
                id: ids[def.symbol.as_str()],
                name: def.segments.last().cloned().unwrap_or_default(),
                segments: def.segments.clone(),
                kind: def.kind,
                line: def.line,
                start: def.start,
                end: def.end,
                test: def.test,
            });
            let refs = doc
                .refs
                .iter()
                .filter_map(|symbol| ids.get(symbol.as_str()).copied());
            (
                doc.path.clone(),
                Doc {
                    defs: defs.collect(),
                    refs: refs.collect(),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, blob: &str) -> Entry {
        Entry {
            path: path.to_string(),
            blob: blob.to_string(),
            size: 10,
            lang: Lang::of(path),
        }
    }

    #[test]
    fn an_indexer_runs_on_the_outermost_projects_with_code_of_its_own() {
        let entries = [
            entry("Cargo.toml", "1"),
            entry("src/main.rs", "2"),
            entry("crates/x/Cargo.toml", "3"),
            entry("crates/x/src/lib.rs", "4"),
            entry("web/package.json", "5"),
            entry("web/tsconfig.json", "6"),
            entry("web/src/app.ts", "7"),
            entry("docs/package.json", "8"),
            entry("vendor/y/go.mod", "9"),
            entry("vendor/y/y.go", "10"),
        ];
        let read: Vec<&Entry> = entries
            .iter()
            .filter(|entry| entry.lang.is_some())
            .collect();
        let settings = IndexSettings::default();
        let root = Path::new("/nowhere");
        let rust = projects(root, &INDEXERS[0], &entries, &read, &settings);
        assert_eq!(
            rust,
            [Project {
                dir: String::new(),
                manifest: "Cargo.toml".into()
            }]
        );
        let typescript = projects(root, &INDEXERS[2], &entries, &read, &settings);
        assert_eq!(
            typescript,
            [Project {
                dir: "web".into(),
                manifest: "web/tsconfig.json".into()
            }]
        );
        assert!(projects(root, &INDEXERS[1], &entries, &read, &settings).is_empty());
    }

    #[test]
    fn an_indexer_runs_again_only_once_a_file_it_reads_changes() {
        let rust = &INDEXERS[0];
        let project = Project {
            dir: String::new(),
            manifest: "Cargo.toml".into(),
        };
        let before = [
            entry("Cargo.toml", "1"),
            entry("src/main.rs", "2"),
            entry("README.md", "3"),
        ];
        let docs_changed = [
            entry("Cargo.toml", "1"),
            entry("src/main.rs", "2"),
            entry("README.md", "4"),
        ];
        let code_changed = [
            entry("Cargo.toml", "1"),
            entry("src/main.rs", "5"),
            entry("README.md", "3"),
        ];
        let key = |entries: &[Entry]| key(rust, "1.0", &project, entries);
        assert_eq!(key(&before), key(&docs_changed));
        assert_ne!(key(&before), key(&code_changed));
        assert_ne!(super::key(rust, "1.1", &project, &before), key(&before));
    }

    #[test]
    fn an_indexer_past_its_time_is_stopped_with_what_it_ran() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & echo started; wait"]);
        let started = Instant::now();
        let ran = bounded(command, &log, Duration::from_millis(300), u64::MAX);
        assert_eq!(ran, Err("took longer than 0s".to_string()));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(fs::read_to_string(&log).unwrap(), "started\n");
        let mut command = Command::new("sh");
        command.args(["-c", "echo nope >&2; exit 3"]);
        let ran = bounded(command, &log, Duration::from_secs(30), u64::MAX);
        assert_eq!(ran, Err("failed (exit status: 3): nope".to_string()));
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let ran = bounded(command, &log, Duration::from_secs(30), 1);
        assert_eq!(ran, Err("took more than 0 MB".to_string()));
    }

    #[test]
    fn a_group_s_memory_is_its_processes_resident_sets() {
        let listed = "    1   8032\n  302   6544\n  302   2112\n 3020  99\n";
        assert_eq!(group_bytes_in(listed, 302), (6544 + 2112) * 1024);
        assert_eq!(group_bytes_in(listed, 7), 0);
        assert!(group_bytes(std::process::id()).is_some());
    }

    #[test]
    fn a_program_that_isn_t_there_or_can_t_say_its_version_isn_t_installed() {
        assert_eq!(locate("crystal-no-such-indexer"), None);
        let dir = tempfile::tempdir().unwrap();
        let broken = dir.path().join("broken");
        fs::write(&broken, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&broken, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(version_of(&broken), None);
        let fine = dir.path().join("fine");
        fs::write(&fine, "#!/bin/sh\necho 'fine 1.2.3'\n").unwrap();
        fs::set_permissions(&fine, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert_eq!(version_of(&fine), Some("fine 1.2.3".to_string()));
    }

    #[test]
    fn what_an_indexer_said_is_kept_named_as_crystal_names_it() {
        let ra = "rust-analyzer cargo crystal 0.3.0 ";
        let index = scip::write::index(
            ("rust-analyzer", "1"),
            &[
                scip::write::document(
                    "src/bell.rs",
                    &[
                        scip::write::occurrence(
                            &[37, 11, 15],
                            &format!("{ra}bell/impl#[Ringer]ring()."),
                            1,
                            &[35, 4, 44, 5],
                        ),
                        scip::write::occurrence(
                            &[40, 4, 8],
                            &format!("{ra}session/Session#"),
                            8,
                            &[],
                        ),
                        scip::write::occurrence(
                            &[41, 4, 8],
                            "rust-analyzer cargo std 1.0 io/stdout().",
                            8,
                            &[],
                        ),
                    ],
                    &[],
                ),
                scip::write::document(
                    "src/session.rs",
                    &[scip::write::occurrence(
                        &[9, 11, 18],
                        &format!("{ra}session/Session#"),
                        1,
                        &[],
                    )],
                    &[scip::write::information(
                        &format!("{ra}session/Session#"),
                        49,
                    )],
                ),
                scip::write::document("src/changed.rs", &[], &[]),
            ],
        );
        let read = scip::read(index.as_slice()).unwrap();
        let project = Project {
            dir: String::new(),
            manifest: "Cargo.toml".into(),
        };
        let checkout = Checkout::Here {
            root: PathBuf::from("/repo"),
            changed: HashSet::from(["src/changed.rs".to_string()]),
        };
        let docs = ids(keep(&read, &project, &checkout));
        assert_eq!(docs.len(), 2);
        let bell = &docs["src/bell.rs"];
        let session = &docs["src/session.rs"];
        assert_eq!(bell.defs[0].segments, ["bell", "Ringer", "ring"]);
        assert_eq!(bell.defs[0].kind, DefKind::Function);
        assert_eq!(
            (bell.defs[0].line, bell.defs[0].start, bell.defs[0].end),
            (38, 36, 45)
        );
        assert_eq!(session.defs[0].kind, DefKind::Struct);
        assert_eq!(bell.refs, [session.defs[0].id]);
    }
}
