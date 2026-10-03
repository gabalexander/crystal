//! The distiller: once a task has closed, a model reads what was done and
//! keeps, in the project's memory, what a later session there would need to
//! know and couldn't find by reading the code.
//!
//! It reads the end of what the task's session did: for a task in the
//! background, Claude's runs as crystal read them; for Claude Code in a
//! terminal, the transcript its hooks named. Codex leaves nothing it can
//! read, so a Codex task is left as its outcome alone.
//!
//! It runs one `claude -p` with nothing a session has: no tools, no MCP
//! servers, none of the project's settings and nobody's hooks, a budget and
//! a turn cap, a system prompt that says what to keep and what never to, and
//! `--json-schema` for the shape of its answer. The answer is checked
//! before anything is kept: kinds it may use, texts short enough, files
//! that are in the checkout, at most [`MAX_DISTILLED`] entries. What passes
//! goes into the memory the way everything does, so what's known already is
//! seen again rather than added twice, and what the user forgot stays
//! forgotten.
//!
//! It never holds anything up: the daemon runs it on a thread of its own,
//! stops it after [`TIMEOUT`], and writes how it went in its log.

use crate::config::MemorySettings;
use crate::embed;
use crate::memory::{self, Added, Kind, New, Source, Store};
use crate::protocol::TaskRecord;
use crate::secrets;
use crate::session::signal_group;
use crate::transcript::{self, Event};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The most entries one pass may keep.
pub const MAX_DISTILLED: usize = 8;

/// How much of what was done the model reads, in bytes: the end of it,
/// where the conclusions are.
pub const MATERIAL_CAP: usize = 48 * 1024;

/// How long one pass may take, start to answer.
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// `--max-turns`. The answer comes back through the tool Claude adds for
/// `--json-schema`, which takes a turn of its own; one more leaves room for
/// it to try again when its answer doesn't fit the schema.
const MAX_TURNS: u32 = 2;

/// The longest an entry's text may be, in characters. Longer, it's refused
/// rather than cut: half a claim can say the opposite of the whole.
const MAX_TEXT: usize = 400;

/// The most files one entry may name.
const MAX_FILES: usize = 8;

/// The longest a line of what was done may be: a tool that printed a page
/// leaves its first few lines.
const MAX_LINE: usize = 2_000;

/// How many of the entries the project has already the model is shown, the
/// ones with most to do with the task, so it doesn't give them again in
/// other words.
const KNOWN_SHOWN: usize = 20;

/// The kinds the distiller may give an entry: how a task turned out is the
/// task's own to say.
const KINDS: [Kind; 4] = [Kind::Decision, Kind::Gotcha, Kind::Command, Kind::Note];

/// Every one of Claude Code's own tools, refused, behind `--tools ""`
/// emptying the list, for a Claude that doesn't know that flag: the model
/// reads what's in its message and nothing else.
const NO_TOOLS: &[&str] = &[
    "Agent",
    "Bash",
    "BashOutput",
    "Edit",
    "Glob",
    "Grep",
    "KillShell",
    "LS",
    "MultiEdit",
    "NotebookEdit",
    "NotebookRead",
    "Read",
    "Task",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
    "Write",
];

/// Settings over the user's own for the pass: none of their hooks run for
/// it. A hook is something the user meant for their sessions, and this
/// isn't one.
const SETTINGS: &str = r#"{"disableAllHooks":true}"#;

/// What the model is told it's doing.
pub const SYSTEM_PROMPT: &str = "You are crystal's memory distiller. You are given the \
record of one finished task in a software project: what it was asked to do, how it ended, and \
what the session that worked on it said and did. Keep only what a later session working in \
this project would need to know and could NOT find by reading the code: decisions and why they \
were made, dead ends and what did not work, commands that work here, surprises and traps. Never \
restate what the code, its comments or its docs already say. Never record progress reports, \
to-do items, or what this session happened to do. Never guess: everything you keep must be \
supported by the record. The message lists what the project's memory has already: never give \
any of that again, even in other words, and keep an entry only when it adds something new.\n\n\
Each entry has: a kind (decision, gotcha, command or note); a text, \
one self-contained statement of at most 300 characters that states the claim itself, with why \
when that matters; and the files it is about, as paths relative to the repository root exactly \
as the record names them (only files the record names; an empty list is fine).\n\nReturn at \
most 8 entries, the most useful first. Return an empty list when nothing qualifies: an empty \
list is better than a weak entry. You have no tools; do not try to read files or run commands: \
everything you may use is in the message. Answer only through the structured output.";

/// The shape of the answer, for `--json-schema`: an object holding the
/// entries, since structured output wants an object at the top.
pub fn schema() -> Value {
    let kinds: Vec<String> = KINDS.iter().map(Kind::to_string).collect();
    json!({
        "type": "object",
        "properties": {
            "entries": {
                "type": "array",
                "maxItems": MAX_DISTILLED,
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": kinds },
                        "text": { "type": "string", "maxLength": MAX_TEXT },
                        "files": {
                            "type": "array",
                            "items": { "type": "string" },
                            "maxItems": MAX_FILES
                        }
                    },
                    "required": ["kind", "text", "files"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["entries"],
        "additionalProperties": false
    })
}

/// Claude's arguments for a pass, its message coming on its standard
/// input. Only the user's own settings are read, for their login and
/// environment, never the checkout's, and nothing it does is kept as a
/// conversation.
pub fn args(settings: &MemorySettings) -> Vec<String> {
    let mut args: Vec<String> = ["-p", "--output-format", "json", "--model"]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
    args.push(settings.distill_model.clone());
    args.extend([
        "--system-prompt".to_string(),
        SYSTEM_PROMPT.to_string(),
        "--json-schema".to_string(),
        schema().to_string(),
        "--tools".to_string(),
        String::new(),
        "--disallowedTools".to_string(),
        NO_TOOLS.join(","),
        "--strict-mcp-config".to_string(),
        "--setting-sources".to_string(),
        "user".to_string(),
        "--settings".to_string(),
        SETTINGS.to_string(),
        "--no-session-persistence".to_string(),
        "--max-turns".to_string(),
        MAX_TURNS.to_string(),
        "--max-budget-usd".to_string(),
        settings.distill_budget_usd.to_string(),
    ]);
    args
}

/// The end of what a session did, a line for each thing it said or did,
/// kept to about [`MATERIAL_CAP`] bytes: the oldest lines go first.
#[derive(Debug, Default, Clone)]
pub struct Record {
    lines: VecDeque<String>,
    bytes: usize,
    /// Whether lines have gone from the start.
    cut: bool,
}

impl Record {
    pub fn push(&mut self, line: String) {
        let line = cut_to(line, MAX_LINE);
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.bytes > MATERIAL_CAP && self.lines.len() > 1 {
            let oldest = self.lines.pop_front().expect("more than one line");
            self.bytes -= oldest.len() + 1;
            self.cut = true;
        }
    }

    /// The lines of one line of what Claude writes: stream-json from
    /// `claude -p`, or a line of the transcript Claude Code keeps.
    pub fn push_claude(&mut self, line: &str) {
        for line in claude_lines(line) {
            self.push(line);
        }
    }

    /// What the transcript Claude Code keeps at `path` says was done.
    pub fn of_transcript(path: &Path) -> Result<Record> {
        let file = File::open(path).with_context(|| format!("couldn't read {}", path.display()))?;
        let mut record = Record::default();
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            record.push_claude(&line);
        }
        Ok(record)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn text(&self) -> String {
        let mut text = String::new();
        if self.cut {
            text.push_str("[… the start is cut; its end follows]\n");
        }
        for line in &self.lines {
            text.push_str(line);
            text.push('\n');
        }
        text
    }
}

/// `line`, cut to its first `max` bytes at a character's edge.
fn cut_to(mut line: String, max: usize) -> String {
    if line.len() > max {
        let mut at = max;
        while !line.is_char_boundary(at) {
            at -= 1;
        }
        line.truncate(at);
        line.push('…');
    }
    line
}

/// What one line of Claude's JSON says was done: what the user asked, what
/// Claude said, the tools it used and what they answered. A subagent's own
/// work is left out, as a task's screen leaves it out.
fn claude_lines(line: &str) -> Vec<String> {
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if event["isSidechain"] == true || event["isMeta"] == true {
        return Vec::new();
    }
    let mut lines = Vec::new();
    if event["type"] == "user" {
        let content = &event["message"]["content"];
        let said: Vec<&str> = match content {
            Value::String(text) => vec![text.as_str()],
            Value::Array(blocks) => blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect(),
            _ => Vec::new(),
        };
        for text in said {
            // What Claude Code writes about the user's own commands, like
            // `/clear`, says nothing about the work.
            if !text.starts_with("<command-") && !text.starts_with("<local-command-") {
                lines.push(format!("USER: {}", text.trim()));
            }
        }
    }
    for event in transcript::events(line) {
        match event {
            Event::Said(text) => lines.push(format!("ASSISTANT: {}", text.trim())),
            Event::UsedTool { name, gist } => lines.push(format!("TOOL {name}: {gist}")),
            Event::ToolAnswered { first_line, failed } => {
                let what = if failed { "ERROR" } else { "RESULT" };
                lines.push(format!("{what}: {first_line}"));
            }
            Event::Started { .. } | Event::Finished(_) => {}
        }
    }
    lines
}

/// What a pass reads.
#[derive(Debug, Clone)]
pub enum Material {
    /// What a task did: the transcript Claude Code keeps of its
    /// conversation, when there is one, being whole; or else what crystal
    /// read of its runs, which a restart loses.
    Task {
        record: Record,
        conversation: Option<String>,
    },
    /// The transcript Claude Code keeps of a conversation in a terminal.
    Transcript(PathBuf),
}

impl Material {
    /// What it says was done. `env` is the session's, which says where
    /// Claude Code keeps its transcripts.
    fn read(&self, env: &BTreeMap<String, String>) -> Result<Record> {
        match self {
            Material::Task {
                record,
                conversation,
            } => {
                let kept = conversation
                    .as_deref()
                    .and_then(|conversation| transcript_of(conversation, env));
                match kept.map(|path| Record::of_transcript(&path)) {
                    Some(Ok(kept)) if !kept.is_empty() => Ok(kept),
                    _ => Ok(record.clone()),
                }
            }
            Material::Transcript(path) => Record::of_transcript(path),
        }
    }
}

/// The transcript Claude Code keeps of `conversation`, in the directory
/// of the project it was in, under its config directory's `projects`.
fn transcript_of(conversation: &str, env: &BTreeMap<String, String>) -> Option<PathBuf> {
    if conversation.is_empty() || conversation.contains(['/', '.']) {
        return None;
    }
    let config = crate::skill::claude_config_dir(
        env.get("CLAUDE_CONFIG_DIR").map(Into::into),
        env.get("HOME").map(Into::into),
    )?;
    let file = format!("{conversation}.jsonl");
    std::fs::read_dir(config.join("projects"))
        .ok()?
        .flatten()
        .map(|project| project.path().join(&file))
        .find(|path| path.is_file())
}

/// One pass, with everything it needs.
#[derive(Debug, Clone)]
pub struct Job {
    pub socket: PathBuf,
    /// The project the memory is kept for: its main worktree.
    pub project: PathBuf,
    /// Where the work was done, which the files an entry names are in.
    pub checkout: PathBuf,
    /// The session that did it, by its name, which the entries say they
    /// came from.
    pub session: String,
    /// What it was asked to do, and how that went.
    pub header: String,
    /// What the work was about, in words to find what the project's memory
    /// has on it already.
    pub about: String,
    pub material: Material,
    /// The session's environment, which Claude runs with.
    pub env: BTreeMap<String, String>,
    pub settings: MemorySettings,
}

/// What the head of a pass's message says of `task`.
pub fn header(task: &TaskRecord) -> String {
    let mut header = format!("The task: {}", task.goal);
    if let Some(outcome) = &task.outcome {
        let how = outcome.state().word();
        header.push_str(&format!("\nHow it ended: {how}: {}", outcome.summary));
    }
    header.push_str(&format!("\nThe project: {}", task.project));
    if let Some(branch) = &task.branch {
        header.push_str(&format!(", on branch {branch}"));
    }
    header
}

/// What `task` was about, to look for in the project's memory: what it was
/// asked, and what it said of how that went.
pub fn about(task: &TaskRecord) -> String {
    match &task.outcome {
        Some(outcome) => format!("{} {}", task.goal, outcome.summary),
        None => task.goal.clone(),
    }
}

/// What a pass came to.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// The ids of the entries it added.
    pub added: Vec<u64>,
    /// The ids of the entries the project had already, seen again.
    pub again: Vec<u64>,
    /// How many it found that the user had forgotten.
    pub forgotten: usize,
    /// The entries the model gave that didn't pass, each with why.
    pub rejected: Vec<String>,
    /// What the pass cost, in US dollars, as Claude counts it.
    pub cost_usd: f64,
}

impl Report {
    /// The report in a line, for a person or the daemon's log.
    pub fn line(&self) -> String {
        let mut line = match self.added.len() {
            1 => "1 entry added".to_string(),
            n => format!("{n} entries added"),
        };
        if !self.again.is_empty() {
            line.push_str(&format!(", {} seen again", self.again.len()));
        }
        if self.forgotten > 0 {
            line.push_str(&format!(", {} forgotten before", self.forgotten));
        }
        if !self.rejected.is_empty() {
            line.push_str(&format!(", {} rejected", self.rejected.len()));
        }
        line.push_str(&format!(" (${:.4})", self.cost_usd));
        line
    }
}

/// Runs a pass, start to end: reads what was done, asks Claude, checks its
/// answer and keeps what passes.
pub fn run(job: &Job) -> Result<Report> {
    let record = job.material.read(&job.env)?;
    if record.is_empty() {
        bail!("there's nothing to read of what was done");
    }
    let mut store = Store::open(&job.socket)?;
    let embedder = embed::shared(&job.settings);
    let embedder = embed::as_embed(&embedder);
    let known = store.search(&job.project, &job.about, None, KNOWN_SHOWN, embedder)?;
    let known: Vec<String> = known
        .iter()
        .map(|entry| format!("({}) {}", entry.kind, memory::one_line(&entry.text)))
        .collect();
    let (answer, cost_usd) = ask_claude(job, &message(&job.header, &known, &record.text()))?;
    let checked = check(&answer, &job.checkout)?;
    let mut report = Report {
        rejected: checked.rejected,
        cost_usd,
        ..Report::default()
    };
    for entry in checked.entries {
        let new = New {
            source: Source::Distilled(job.session.clone()),
            ..entry
        };
        match store.add(&job.project, new)? {
            Added::New(entry) => report.added.push(entry.id),
            Added::Again(entry) => report.again.push(entry.id),
            Added::Refused => report.forgotten += 1,
        }
    }
    Ok(report)
}

/// The message a pass sends: what the work was and how it ended, what the
/// project's memory has already that has to do with it, `known`, then what
/// was done. Credentials are taken out of all of it, so the model never
/// sees a key a session printed, to copy it into an entry.
pub fn message(header: &str, known: &[String], record: &str) -> String {
    let mut message = secrets::redact(header);
    message.push_str("\n\nWhat the project's memory has already");
    if known.is_empty() {
        message.push_str(": nothing on this yet.");
    } else {
        message.push_str(" (never give any of it again, even in other words):\n");
        for entry in known {
            message.push_str(&format!("- {}\n", secrets::redact(entry)));
        }
    }
    message.push_str(&format!(
        "\n\nThe record (oldest first; the start may be cut):\n\n{}\nReturn the entries a \
         later session in this project would need.",
        secrets::redact(record),
    ));
    message
}

/// Runs Claude on `message`, and gives back its answer, the structured
/// output, and what it cost.
fn ask_claude(job: &Job, message: &str) -> Result<(Value, f64)> {
    let mut env = job.env.clone();
    // It isn't a session, and its hooks are off anyway.
    for key in ["CRYSTAL_SESSION", "CRYSTAL_SESSION_ID"] {
        env.remove(key);
    }
    let mut child = Command::new("claude")
        .args(args(&job.settings))
        .current_dir(&job.checkout)
        .env_clear()
        .envs(&env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A process group of its own, to stop all of it when it takes too
        // long.
        .process_group(0)
        .spawn()
        .context("couldn't start claude")?;
    let pid = child.id();
    let mut stdin = child.stdin.take().expect("its input is piped");
    let message = message.to_string();
    thread::spawn(move || {
        // Closed once written: that's the end of the prompt.
        let _ = stdin.write_all(message.as_bytes());
    });
    let read_all = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let stdout = read_all(child.stdout.take().map(|pipe| Box::new(pipe) as _));
    let stderr = read_all(child.stderr.take().map(|pipe| Box::new(pipe) as _));
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            signal_group(pid, libc::SIGKILL);
            let _ = child.wait();
            bail!(
                "it took longer than {}s, and was stopped",
                TIMEOUT.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }
    let out = stdout.join().unwrap_or_default();
    let errors = stderr.join().unwrap_or_default();
    answer_of(&out).with_context(
        || match errors.lines().rev().find(|l| !l.trim().is_empty()) {
            Some(error) => format!("claude said: {}", error.trim()),
            None => "claude ended without an answer".to_string(),
        },
    )
}

/// The answer in what `claude -p --output-format json` wrote, and what it
/// cost; or what went wrong.
fn answer_of(out: &str) -> Result<(Value, f64)> {
    let result = out
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|value| value["type"] == "result")
        .context("there's no result in what it wrote")?;
    let cost = result["total_cost_usd"].as_f64().unwrap_or(0.0);
    if result["is_error"] == true || result["subtype"] != "success" {
        let subtype = result["subtype"].as_str().unwrap_or("error");
        let said = result["result"].as_str().unwrap_or_default();
        bail!("its run ended with {subtype} (${cost:.4}): {said}");
    }
    let answer = match &result["structured_output"] {
        Value::Null => result["result"]
            .as_str()
            .and_then(|text| serde_json::from_str(text.trim()).ok())
            .context("its answer had no structured output")?,
        answer => answer.clone(),
    };
    Ok((answer, cost))
}

/// The model's entries that passed, and why each that didn't, didn't.
#[derive(Debug, Default, PartialEq)]
pub struct Checked {
    pub entries: Vec<New>,
    pub rejected: Vec<String>,
}

/// Checks the model's answer against the checkout: an array of entries,
/// bare or as `entries`, at most [`MAX_DISTILLED`]; each of a kind the
/// distiller may give, with a text that isn't empty or too long, naming
/// files that are in `checkout`. An entry that fails any of it is dropped
/// whole, with why.
pub fn check(answer: &Value, checkout: &Path) -> Result<Checked> {
    let items = match answer {
        Value::Array(items) => items,
        Value::Object(object) => match object.get("entries") {
            Some(Value::Array(items)) => items,
            _ => bail!("its answer has no entries"),
        },
        _ => bail!("its answer isn't an object with entries"),
    };
    let mut checked = Checked::default();
    for (at, item) in items.iter().enumerate() {
        let number = at + 1;
        if at >= MAX_DISTILLED {
            checked
                .rejected
                .push(format!("entry {number}: past the first {MAX_DISTILLED}"));
            continue;
        }
        match entry(item, checkout) {
            Ok(entry) => checked.entries.push(entry),
            Err(why) => checked.rejected.push(format!("entry {number}: {why}")),
        }
    }
    Ok(checked)
}

fn entry(item: &Value, checkout: &Path) -> Result<New, String> {
    let kind = item["kind"].as_str().unwrap_or_default();
    let kind = Kind::parse(kind)
        .filter(|kind| KINDS.contains(kind))
        .ok_or_else(|| format!("{kind:?} isn't a kind it may give"))?;
    let text = item["text"].as_str().unwrap_or_default().trim();
    if text.is_empty() {
        return Err("it says nothing".to_string());
    }
    if text.chars().count() > MAX_TEXT {
        return Err(format!("it's longer than {MAX_TEXT} characters"));
    }
    let mut files = Vec::new();
    let named = item["files"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for file in named {
        let file = file.as_str().ok_or("a file that isn't a path")?;
        let found =
            file_in(checkout, file).ok_or_else(|| format!("{file} isn't in the checkout"))?;
        if !files.contains(&found) {
            files.push(found);
        }
    }
    if files.len() > MAX_FILES {
        return Err(format!("it names more than {MAX_FILES} files"));
    }
    Ok(New {
        kind,
        text: text.to_string(),
        files,
        // Who it's from is the pass's to say.
        source: Source::User,
        checkout: Some(checkout.to_path_buf()),
    })
}

/// `file`, as a model named it, from the top of `checkout`, if it's there:
/// relative, or absolute but inside it, and never above it.
fn file_in(checkout: &Path, file: &str) -> Option<String> {
    let file = file.trim();
    let path = Path::new(file.strip_prefix("./").unwrap_or(file));
    let relative = if path.is_absolute() {
        let canonical = checkout.canonicalize().ok();
        path.strip_prefix(checkout)
            .ok()
            .or_else(|| path.strip_prefix(canonical.as_ref()?).ok())?
            .to_path_buf()
    } else {
        path.to_path_buf()
    };
    let plain = relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)));
    if relative.as_os_str().is_empty() || !plain || !checkout.join(&relative).exists() {
        return None;
    }
    Some(relative.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{TaskOutcome, TaskState};

    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/ledger.rs"), "").unwrap();
        dir
    }

    #[test]
    fn the_pass_has_no_tools_no_servers_no_hooks_and_a_budget() {
        let settings = MemorySettings::default();
        let args = args(&settings);
        let after = |flag: &str| {
            let at = args.iter().position(|arg| arg == flag).unwrap();
            args[at + 1].clone()
        };
        assert_eq!(args[..3], ["-p", "--output-format", "json"]);
        assert_eq!(after("--model"), "claude-haiku-4-5");
        assert_eq!(after("--tools"), "");
        assert!(after("--disallowedTools").contains("Bash"));
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert_eq!(after("--setting-sources"), "user");
        assert_eq!(after("--settings"), r#"{"disableAllHooks":true}"#);
        assert!(args.contains(&"--no-session-persistence".to_string()));
        assert_eq!(after("--max-budget-usd"), "0.25");
        let schema: Value = serde_json::from_str(&after("--json-schema")).unwrap();
        assert_eq!(schema["properties"]["entries"]["maxItems"], 8);
    }

    #[test]
    fn a_record_keeps_the_end_of_what_was_done() {
        let mut record = Record::default();
        for at in 0..10_000 {
            record.push(format!("ASSISTANT: line {at}"));
        }
        let text = record.text();
        assert!(text.len() <= MATERIAL_CAP + 100);
        assert!(text.starts_with("[… the start is cut"));
        assert!(text.ends_with("ASSISTANT: line 9999\n"));
    }

    #[test]
    fn a_line_too_long_keeps_its_start() {
        let mut record = Record::default();
        record.push(format!("RESULT: {}", "é".repeat(5_000)));
        let text = record.text();
        assert!(text.len() < MAX_LINE + 10);
        assert!(text.trim_end().ends_with('…'));
    }

    #[test]
    fn a_transcript_s_lines_say_who_did_what() {
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"fix the ledger"}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"Caveat: local commands"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"Running the tests."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"error: redis refused","is_error":true}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"a subagent"}]}}"#,
            r#"{"type":"summary","summary":"Ledger"}"#,
            "not json",
        ];
        let mut record = Record::default();
        for line in lines {
            record.push_claude(line);
        }
        assert_eq!(
            record.text(),
            "USER: fix the ledger\n\
             ASSISTANT: Running the tests.\n\
             TOOL Bash: cargo test\n\
             ERROR: error: redis refused\n"
        );
    }

    #[test]
    fn the_message_says_what_the_task_was_and_takes_out_credentials() {
        let task = TaskRecord {
            id: Some(1),
            goal: "fix the ledger".into(),
            session: "fixer".into(),
            project: "app".into(),
            branch: Some("fix/ledger".into()),
            background: true,
            backlog: None,
            pending: false,
            waiting: false,
            created: 0,
            outcome: Some(TaskOutcome::new(TaskState::Done, "redis has to be up", 0)),
        };
        let header = header(&task);
        assert_eq!(
            header,
            "The task: fix the ledger\nHow it ended: done: redis has to be up\n\
             The project: app, on branch fix/ledger"
        );
        let known = ["(gotcha) redis has to be up".to_string()];
        let record = "TOOL Bash: REDIS_PASSWORD=hunter22x make\n";
        let message = message(&header, &known, record);
        assert!(
            message.contains(
                "What the project's memory has already (never give any of it again, even in \
                 other words):\n- (gotcha) redis has to be up\n"
            ),
            "{message}"
        );
        assert!(message.contains("REDIS_PASSWORD=[redacted]"), "{message}");
        assert!(!message.contains("hunter22x"));
        let message = super::message(&header, &[], record);
        assert!(
            message.contains("has already: nothing on this yet."),
            "{message}"
        );
    }

    #[test]
    fn the_answer_is_its_structured_output_and_its_cost() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"entries":[]},"total_cost_usd":0.012}"#;
        let (answer, cost) = answer_of(out).unwrap();
        assert_eq!(answer, json!({"entries": []}));
        assert_eq!(cost, 0.012);

        // An older Claude puts it in its result, as text.
        let out =
            r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"entries\":[]}"}"#;
        assert_eq!(answer_of(out).unwrap().0, json!({"entries": []}));

        let out = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"total_cost_usd":0.26}"#;
        let err = answer_of(out).unwrap_err().to_string();
        assert!(err.contains("error_max_budget_usd"), "{err}");
        assert!(answer_of("Error: not logged in").is_err());
    }

    #[test]
    fn entries_that_pass_are_kept_and_the_rest_say_why() {
        let dir = checkout();
        let answer = json!({"entries": [
            {"kind": "gotcha", "text": "The ledger tests need redis up", "files": ["./src/ledger.rs"]},
            {"kind": "outcome", "text": "done", "files": []},
            {"kind": "note", "text": "  ", "files": []},
            {"kind": "note", "text": "x".repeat(401), "files": []},
            {"kind": "decision", "text": "Fees in cents", "files": ["src/fees.rs"]},
            {"kind": "note", "text": "Escapes", "files": ["../secrets.txt"]},
            {"kind": "command", "text": "make ci runs it all",
             "files": [dir.path().join("src/ledger.rs").to_str().unwrap()]},
        ]});
        let checked = check(&answer, dir.path()).unwrap();
        let texts: Vec<&str> = checked.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            ["The ledger tests need redis up", "make ci runs it all"]
        );
        assert_eq!(checked.entries[0].files, ["src/ledger.rs"]);
        assert_eq!(checked.entries[1].files, ["src/ledger.rs"]);
        assert_eq!(checked.rejected.len(), 5);
        assert!(checked.rejected[0].contains("\"outcome\""));
        assert!(checked.rejected[3].contains("src/fees.rs isn't in the checkout"));
        assert!(checked.rejected[4].contains("../secrets.txt"));
    }

    #[test]
    fn no_more_than_eight_are_kept() {
        let dir = checkout();
        let entries: Vec<Value> = (0..10)
            .map(|at| json!({"kind": "note", "text": format!("note {at}"), "files": []}))
            .collect();
        let checked = check(&Value::Array(entries), dir.path()).unwrap();
        assert_eq!(checked.entries.len(), MAX_DISTILLED);
        assert_eq!(checked.rejected.len(), 2);
        assert!(check(&json!("nothing"), dir.path()).is_err());
    }

    #[test]
    fn a_task_is_read_from_the_transcript_claude_keeps_when_it_has_one() {
        let config = tempfile::tempdir().unwrap();
        let kept = config.path().join("projects/-code-app");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(
            kept.join("conv-1.jsonl"),
            r#"{"type":"user","message":{"content":"fix the ledger"}}"#,
        )
        .unwrap();
        let env = BTreeMap::from([(
            "CLAUDE_CONFIG_DIR".to_string(),
            config.path().display().to_string(),
        )]);
        let mut record = Record::default();
        record.push("USER: only the last run".into());
        let task = |conversation: &str| Material::Task {
            record: record.clone(),
            conversation: Some(conversation.into()),
        };
        assert_eq!(
            task("conv-1").read(&env).unwrap().text(),
            "USER: fix the ledger\n"
        );
        assert_eq!(
            task("conv-2").read(&env).unwrap().text(),
            "USER: only the last run\n"
        );
        assert_eq!(
            task("../conv-1").read(&env).unwrap().text(),
            "USER: only the last run\n"
        );
    }

    #[test]
    fn a_report_says_what_came_of_the_pass_in_a_line() {
        let report = Report {
            added: vec![4, 5],
            again: vec![1],
            forgotten: 1,
            rejected: vec!["entry 3: it says nothing".into()],
            cost_usd: 0.0123,
        };
        assert_eq!(
            report.line(),
            "2 entries added, 1 seen again, 1 forgotten before, 1 rejected ($0.0123)"
        );
    }
}
