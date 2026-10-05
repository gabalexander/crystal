//! The distiller: once a task has closed, done or failed, or a session is
//! archived without its task having closed so, a model reads what was done
//! and keeps, in the project's memory, what a later session there would
//! need to know and couldn't find by reading the code.
//!
//! It's also shown a few of the entries gone stale that are about the files
//! the work touched, and says of each that the record settles whether it
//! still holds as it is, holds reworded, or no longer holds: the entry is
//! anchored again, reworded or forgotten.
//!
//! What it keeps may correct an entry it was shown the memory has: then it
//! says which, and why, and whether what it keeps is that entry corrected,
//! put in its place under its id, or a new entry the old one is retired
//! for. And with no record to read, it looks through groups of entries near
//! one another in meaning for those another in the group shows no longer
//! hold, for `crystal memory reconcile` ([`superseded_among`]).
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
//! that are in the checkout, at most [`MAX_DISTILLED`] entries, and only
//! the stale entries it was shown, one kept or reworded naming something
//! that's in the checkout when it names anything; only entries it was shown
//! replaced, each once, with why, by an entry naming something that's in the
//! checkout when it names anything. What passes
//! goes into the memory the way everything does, so what's known already is
//! seen again rather than added twice, and what the user forgot stays
//! forgotten.
//!
//! It never holds anything up: the daemon runs it on a thread of its own,
//! stops it after [`TIMEOUT`], and writes how it went in its log.

use crate::config::MemorySettings;
use crate::embed;
use crate::handover::HELPERS;
use crate::memory::{self, Added, Entry, Kind, Listed, New, Source, Store, Superseded};
use crate::output::errln;
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
/// other words, and can tell which of the notes among them are lessons.
const KNOWN_SHOWN: usize = 20;

/// How many of the stale entries about the files the work touched the
/// model is asked about, the ones said most recently.
pub const RECHECKED: usize = 4;

/// How many notes one pass over notes alone reads: few enough that it
/// weighs each.
const NOTES_AT_ONCE: usize = 40;

/// How many of the last things the session said the entries the model is
/// shown are found nearest to: where the conclusions it would keep are.
const SAID_LOOKED_AT: usize = 16;

/// The longest the reason an entry no longer holds may be, in characters:
/// a sentence.
const MAX_WHY: usize = 300;

/// How many entries one pass over groups of entries near one another reads,
/// at most: their groups whole. The model thinks each through, and passes
/// of 40 took two to three minutes on crystal's own memory.
const NEAR_AT_ONCE: usize = 24;

/// How many passes over groups of entries near one another run at once.
const NEAR_PASSES_AT_ONCE: usize = 4;

/// How long one pass over groups of entries near one another may take:
/// longer than the distiller's [`TIMEOUT`], as it weighs each entry against
/// the others of its group. Without thinking it's quick, but takes nearly
/// every older entry for one the newer replaces.
const NEAR_TIMEOUT: Duration = Duration::from_secs(300);

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

/// What the model is told it's doing: what to keep, what never to, with
/// entries of each that crystal's own memory kept before these rules, and
/// never to put words in the user's mouth.
pub const SYSTEM_PROMPT: &str = "You are crystal's memory distiller. You are given the \
record of one finished task in a software project: what it was asked to do, how it ended, and \
what the session that worked on it said and did. Keep only lessons a later session working in \
this project would need and could NOT get from the code, its comments and docs, the git log, \
the project's backlog or its CLAUDE.md: decisions and why they were made, dead ends and what \
did not work, commands that work here, surprises and traps.\n\n\
Never keep:\n\
- progress or status: what was merged, pushed, committed, installed or released, that CI \
passed, what this session did or still has to do;\n\
- a commit hash, or a pull request, issue or backlog number as the point of an entry;\n\
- anything only true today: what isn't done yet, what is tracked where, what waits on what;\n\
- what the code, its comments, its docs or the git log already say.\n\
For example, none of these is a lesson: \"PR #56 squash-merged into master as 8012b6c\"; \"PRs \
#99-#104 all merged but the binary is not yet installed; run make install\"; \"Backlog #81 \
tracks showing removal progress to other clients\"; \"Flaky plugin test archived to backlog \
#135 during the merge\"; \"Text boxes now share src/tui/editing.rs\". These are: \"The ledger \
tests need the database up: run make db first\"; \"Fees are kept in cents, since a float lost \
a cent in a refund\".\n\n\
Never guess: everything you keep must be supported by the record. Never say what the user \
decided, wants or prefers unless the record shows the user saying it (a USER: line); what the \
assistant proposed or did is not the user's decision, and never turn a question into an \
answer. Quote names exactly as the record gives them (files, functions, tests, commands, \
settings), and give no line numbers. The message lists what the project's memory has already, \
each by its id and kind: never give any of that again, even in other words, and keep an entry \
only when it adds something new. When one of those that is a note is in fact a lesson, give its \
id in kinds with the kind it should have (decision, gotcha or command), and leave out every \
other.\n\n\
When the record shows that one of those no longer holds as it says, because an entry you keep \
corrects or replaces it (a default or a behaviour that changed, a decision reversed, a command, a \
file or a name renamed, a limit changed, a workaround a fix made unnecessary), say so in that \
entry's replaces: the id of the one that no longer holds; how, update when yours is that entry \
corrected (yours is put in its place, under its id) or retire when yours is a new statement that \
makes it obsolete (it is retired, and yours is added); and why, one sentence on what in the \
record shows it no longer holds. This is the only way to say that one of the memory's entries no \
longer holds: keep the entry that says what holds now, with replaces naming the old one. replaces \
is null for every other entry: never replace one that still holds, one that says what yours does \
(leave yours out instead), or one the record says nothing about.\n\n\
Each entry has: a kind (decision, gotcha or command for a lesson; note only for a lasting \
fact that is none of those, and notes nobody finds again expire); a text, one self-contained \
statement of at most 300 characters that states the claim itself, with why when that matters; \
and the files it is about, as paths relative to the repository root exactly as the record \
names them (only files the record names; an empty list is fine).\n\nReturn at most 8 entries, \
the most useful first. Return an empty list when nothing qualifies: an empty list is better \
than a weak entry.\n\nThe message may also list entries the memory has already that may no \
longer hold, each by its id, with what it names that is gone from the code. For each one the \
record settles, give a verdict in rechecked: keep when it still holds as it says, with an empty \
text; reword when it holds once corrected, with the corrected statement as its text, under the \
same rules as an entry's; forget when it no longer holds, with an empty text. Leave out every \
one the record does not settle: never guess. rechecked is for those listed as possibly no longer \
holding alone, and is empty when none is listed.\n\nYou have no tools; do not try to read files or \
run commands: everything you may use is in the message. Answer only through the structured \
output.";

/// What the model is told as it reads a project's notes alone, for the
/// lessons among them: see [`lessons_among`]. Told only to look for
/// lessons, it took descriptions of the code and progress for them too,
/// and called any instruction a command, so it's told what is neither.
pub const NOTES_PROMPT: &str = "You are crystal's memory distiller. You are given notes from \
a software project's memory, each after its id. Some are lessons kept as notes by mistake. A \
lesson tells a later session something it would otherwise get wrong, and couldn't see by \
reading the code:\n\
- a decision: a choice made and why, where another would look as good (\"Fees are kept in \
cents, since a float lost a cent in a refund\");\n\
- a gotcha: a trap or a surprise, something that fails or misleads unless you know it (\"The \
ledger tests fail unless the database is up\");\n\
- a command: a command line to run that does something useful here (\"make db starts the \
database the tests need\").\n\
These are not lessons, and stay notes: what the code does or where something lives (\"The \
sidebar's width is kept in the ui table\"), what was changed or added (\"Text boxes now share \
one editing module\"), progress or status, measurements and how something was found, and \
anything only true when it was written.\n\n\
Say which notes are lessons, each by its id with the kind it should have. When unsure, leave \
it out: a note left a note loses nothing. You have no tools; do not try to read files or run \
commands: everything you may use is in the message. Answer only through the structured output.";

/// What the model is told as it looks through groups of entries near one
/// another for those that no longer hold: see [`superseded_among`]. Most
/// near one another all hold, so it's told what doesn't make one stop
/// holding, and to leave out what it's unsure of.
pub const RECONCILE_PROMPT: &str = "You are crystal's memory distiller. You are given groups of \
entries from a software project's memory; the entries in a group are near one another in \
meaning. Each is given by its id, its kind, how long ago it was last said and, when some of what \
it names is gone from the code, what is gone. Ids count up as entries are added, so a higher id \
was first said later.\n\n\
Most entries near one another all still hold: they say different things about the same subject, \
or the same thing in other words, which is not for you. Find only those that no longer hold \
because another entry in their group shows it: a default or a behaviour that changed, a decision \
reversed, a command, a file or a name renamed, a limit or a count that changed, a workaround that \
a later fix made unnecessary. The later entry usually holds; what is gone from the code tells \
too.\n\n\
For each, give its id, and how:\n\
- retire, with by: the id of the entry in the same group that holds in its place, and an empty \
text, when that one says what holds now and all of this one that still holds;\n\
- update, with by null and text: the entry corrected, one self-contained statement of at most \
300 characters, when some of it still holds that no other entry of its group says: keep that, \
and correct the rest; use only what the group's entries say;\n\
and why: one sentence on what in the group shows it no longer holds.\n\n\
Never retire an entry because another says the same thing, says more about the same subject, or \
is newer: only when what it says is no longer true. When unsure, leave it out: an entry left as \
it is loses nothing, one retired wrongly is hidden from every later session. Return an empty list \
when every entry holds. You have no tools; do not try to read files or run commands: everything \
you may use is in the message. Answer only through the structured output.";

/// The kinds a note that's a lesson may be given.
const LESSONS: [Kind; 3] = [Kind::Decision, Kind::Gotcha, Kind::Command];

/// The shape of a note made a lesson in an answer: its id, and its kind.
fn lesson_schema() -> Value {
    let kinds: Vec<String> = LESSONS.iter().map(Kind::to_string).collect();
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer" },
            "kind": { "type": "string", "enum": kinds }
        },
        "required": ["id", "kind"],
        "additionalProperties": false
    })
}

/// The shape of the answer, for `--json-schema`: an object holding the
/// entries, since structured output wants an object at the top, and the
/// notes it was shown that are lessons.
pub fn schema() -> Value {
    let kinds: Vec<String> = KINDS.iter().map(Kind::to_string).collect();
    json!({
        "type": "object",
        "properties": {
            "kinds": {
                "type": "array",
                "maxItems": KNOWN_SHOWN,
                "items": lesson_schema()
            },
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
                        },
                        "replaces": {
                            "anyOf": [
                                { "type": "null" },
                                {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "integer" },
                                        "how": { "type": "string", "enum": ["update", "retire"] },
                                        "why": { "type": "string", "maxLength": MAX_WHY }
                                    },
                                    "required": ["id", "how", "why"],
                                    "additionalProperties": false
                                }
                            ]
                        }
                    },
                    "required": ["kind", "text", "files", "replaces"],
                    "additionalProperties": false
                }
            },
            "rechecked": {
                "type": "array",
                "maxItems": RECHECKED,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "verdict": { "type": "string", "enum": ["keep", "reword", "forget"] },
                        "text": { "type": "string", "maxLength": MAX_TEXT }
                    },
                    "required": ["id", "verdict", "text"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["entries", "kinds", "rechecked"],
        "additionalProperties": false
    })
}

/// The shape of the answer of a pass over groups of entries near one
/// another: the entries that no longer hold, each retired for another of
/// its group or updated, and why.
pub fn reconcile_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "superseded": {
                "type": "array",
                "maxItems": NEAR_AT_ONCE,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "how": { "type": "string", "enum": ["retire", "update"] },
                        "by": { "anyOf": [{ "type": "null" }, { "type": "integer" }] },
                        "text": { "type": "string", "maxLength": MAX_TEXT },
                        "why": { "type": "string", "maxLength": MAX_WHY }
                    },
                    "required": ["id", "how", "by", "text", "why"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["superseded"],
        "additionalProperties": false
    })
}

/// The shape of the answer of a pass over notes alone.
pub fn notes_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "lessons": {
                "type": "array",
                "maxItems": NOTES_AT_ONCE,
                "items": lesson_schema()
            }
        },
        "required": ["lessons"],
        "additionalProperties": false
    })
}

/// Claude's arguments for a pass, its message coming on its standard
/// input. Only the user's own settings are read, for their login and
/// environment, never the checkout's, and nothing it does is kept as a
/// conversation.
pub fn args(settings: &MemorySettings) -> Vec<String> {
    pass_args(settings, SYSTEM_PROMPT, &schema())
}

/// [`args`], for a pass told `prompt` and answering in the shape of
/// `schema`.
fn pass_args(settings: &MemorySettings, prompt: &str, schema: &Value) -> Vec<String> {
    let mut args: Vec<String> = ["-p", "--output-format", "json", "--model"]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
    args.push(settings.distill_model.clone());
    args.extend([
        "--system-prompt".to_string(),
        prompt.to_string(),
        "--json-schema".to_string(),
        schema.to_string(),
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
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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

    /// The files it says were edited or written, as the tools named them.
    pub fn edited(&self) -> Vec<&str> {
        let edits = ["Edit", "MultiEdit", "NotebookEdit", "Write"];
        self.lines
            .iter()
            .filter_map(|line| {
                let (tool, file) = line.strip_prefix("TOOL ")?.split_once(": ")?;
                edits.contains(&tool).then_some(file)
            })
            .collect()
    }

    /// The last `most` things the session said, the latest last: what the
    /// entries the model gives come from.
    pub fn said(&self, most: usize) -> Vec<&str> {
        let said: Vec<&str> = self
            .lines
            .iter()
            .filter_map(|line| line.strip_prefix("ASSISTANT: "))
            .filter(|said| !said.trim().is_empty())
            .collect();
        said[said.len().saturating_sub(most)..].to_vec()
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
    let said = transcript::kept_events(line).into_iter();
    said.filter_map(|event| match event {
        Event::Asked(text) => Some(format!("USER: {text}")),
        Event::Said(text) => Some(format!("ASSISTANT: {}", text.trim())),
        Event::UsedTool { name, gist } => Some(format!("TOOL {name}: {gist}")),
        Event::ToolAnswered { first_line, failed } => {
            let what = if failed { "ERROR" } else { "RESULT" };
            Some(format!("{what}: {first_line}"))
        }
        Event::Started { .. } | Event::Context { .. } | Event::Finished(_) => None,
    })
    .collect()
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
pub fn transcript_of(conversation: &str, env: &BTreeMap<String, String>) -> Option<PathBuf> {
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "DistillReport"))]
pub struct Report {
    /// The ids of the entries it added.
    pub added: Vec<u64>,
    /// The ids of the entries the project had already, seen again.
    pub again: Vec<u64>,
    /// How many it found that the user had forgotten.
    pub forgotten: usize,
    /// The ids of the stale entries it said still hold, anchored again.
    #[serde(default)]
    pub kept: Vec<u64>,
    /// The ids of the stale entries it reworded.
    #[serde(default)]
    pub reworded: Vec<u64>,
    /// The ids of the stale entries it said no longer hold, forgotten.
    #[serde(default)]
    pub forgot: Vec<u64>,
    /// Those entries as they were, for the daemon to tell of.
    #[serde(skip)]
    #[cfg_attr(test, schemars(skip))]
    pub forgot_entries: Vec<Entry>,
    /// The ids of the entries it was shown that what it kept corrected,
    /// each put in its place under its id.
    #[serde(default)]
    pub updated: Vec<u64>,
    /// The ids of the entries it was shown that what it kept replaced, each
    /// retired.
    #[serde(default)]
    pub retired: Vec<u64>,
    /// Each entry updated, retired or reworded, as it was, with the entry
    /// that holds in its place as it is now, for the daemon to tell of.
    #[serde(skip)]
    #[cfg_attr(test, schemars(skip))]
    pub superseded: Vec<(Superseded, Entry)>,
    /// The entries the model gave that didn't pass, each with why.
    pub rejected: Vec<String>,
    /// What the pass cost, in US dollars, as Claude counts it.
    pub cost_usd: f64,
    /// The ids of the notes it was shown that it made lessons.
    #[serde(default)]
    pub made_lessons: Vec<u64>,
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
        match self.made_lessons.len() {
            0 => {}
            1 => line.push_str(", 1 note made a lesson"),
            n => line.push_str(&format!(", {n} notes made lessons")),
        }
        if !self.updated.is_empty() {
            line.push_str(&format!(", {} updated", self.updated.len()));
        }
        if !self.retired.is_empty() {
            line.push_str(&format!(", {} retired", self.retired.len()));
        }
        let rechecked = [
            (self.kept.len(), "kept"),
            (self.reworded.len(), "reworded"),
            (self.forgot.len(), "forgotten"),
        ];
        let rechecked: Vec<String> = (rechecked.iter())
            .filter(|(count, _)| *count > 0)
            .map(|(count, what)| format!("{count} {what}"))
            .collect();
        if !rechecked.is_empty() {
            line.push_str(&format!(", of the stale {}", rechecked.join(", ")));
        }
        if !self.rejected.is_empty() {
            line.push_str(&format!(", {} rejected", self.rejected.len()));
        }
        line.push_str(&format!(" (${:.4})", self.cost_usd));
        line
    }
}

/// Runs a pass, start to end: reads what was done, asks Claude, checks its
/// answer and keeps what passes, each that corrects an entry it was shown
/// in that one's place, does as it says with the stale entries it was
/// shown, and gives the notes it was shown that it says are lessons their
/// kinds.
pub fn run(job: &Job) -> Result<Report> {
    let record = job.material.read(&job.env)?;
    if record.is_empty() {
        bail!("there's nothing to read of what was done");
    }
    let mut store = Store::open(&job.socket)?;
    let embedder = embed::shared(&job.settings);
    let embedder = embed::as_embed(&embedder);
    let known = known(&mut store, job, &record, embedder)?;
    // A note that reads as status is no lesson: `list --status` has it.
    let notes: Vec<u64> = known
        .iter()
        .filter(|entry| entry.kind == Kind::Note && !memory::reads_as_status(&entry.text))
        .map(|entry| entry.id)
        .collect();
    // How a task turned out is history, and nothing replaces it.
    let replaceable: Vec<u64> = (known.iter())
        .filter(|entry| entry.kind != Kind::Outcome)
        .map(|entry| entry.id)
        .collect();
    let known: Vec<String> = known
        .iter()
        .map(|entry| {
            let text = memory::one_line(&entry.text);
            format!("{} ({}) {text}", entry.id, entry.kind)
        })
        .collect();
    let touched = touched(&job.checkout, &record);
    let stale = store.stale_about(&job.project, &touched, RECHECKED)?;
    let message = message(&job.header, &known, &stale, &record.text());
    let (answer, cost_usd) = ask_claude(job, &message)?;
    let checked = check(&answer, &job.checkout, &stale, &notes, &replaceable)?;
    let mut report = Report {
        rejected: checked.rejected,
        cost_usd,
        ..Report::default()
    };
    // Before the rest, which could be taken for one of those it replaces
    // said again.
    for replacement in checked.replacements {
        let id = replacement.id;
        if let Err(err) = replace(&mut store, job, replacement, embedder, &mut report) {
            report.rejected.push(format!("entry {id}: {err:#}"));
        }
    }
    for entry in checked.entries {
        let new = New {
            source: Source::Distilled(job.session.clone()),
            ..entry
        };
        match store.add_with(&job.project, new, embedder)? {
            Added::New(entry) | Added::Near { entry, .. } => report.added.push(entry.id),
            Added::Again(entry) | Added::Alike(entry) => report.again.push(entry.id),
            Added::Outdated(_) | Added::Refused => report.forgotten += 1,
        }
    }
    for Recheck { id, verdict } in checked.rechecks {
        let done = match verdict {
            Verdict::Keep => {
                (store.reanchor(&job.project, id, &job.checkout)).map(|_| report.kept.push(id))
            }
            Verdict::Reword(text) => {
                (store.reword(&job.project, id, &text, &job.checkout)).map(|reworded| {
                    report.reworded.push(id);
                    report.superseded.push(reworded);
                })
            }
            Verdict::Forget => store.remove(&job.project, id).map(|entry| {
                report.forgot.push(id);
                report.forgot_entries.push(entry);
            }),
        };
        if let Err(err) = done {
            report.rejected.push(format!("entry {id}: {err:#}"));
        }
    }
    for (id, kind) in checked.lessons {
        // Someone may have changed it since it was read, or the pass forgot it.
        if store
            .get(&job.project, id)?
            .is_some_and(|entry| entry.kind == Kind::Note)
        {
            store.set_kind(&job.project, id, kind)?;
            report.made_lessons.push(id);
        }
    }
    Ok(report)
}

/// Keeps what `replacement` says in place of the entry it replaces, as the
/// distiller of `job` said it: that entry corrected, under its id, or a new
/// entry (or one said before, seen again), the one it replaces retired.
fn replace(
    store: &mut Store,
    job: &Job,
    replacement: Replacement,
    embedder: Option<&dyn embed::Embed>,
    report: &mut Report,
) -> Result<()> {
    let Replacement {
        id,
        how,
        why,
        entry,
    } = replacement;
    let new = New {
        source: Source::Distilled(job.session.clone()),
        ..entry
    };
    let project = &job.project;
    match how {
        How::Update => {
            let checkout = &job.checkout;
            let kind = Some(new.kind);
            let (was, now) =
                store.update(project, id, &new.text, kind, &new.files, checkout, &why)?;
            report.updated.push(id);
            report.superseded.push((was, now));
        }
        How::Retire => {
            let (added, retired) = store.replace(project, new, id, &why, embedder)?;
            let (Some(retired), Some(holder)) = (retired, added.entry().cloned()) else {
                bail!("what replaces it was forgotten, so it stays");
            };
            match added {
                Added::New(_) | Added::Near { .. } => report.added.push(holder.id),
                _ => report.again.push(holder.id),
            }
            report.retired.push(id);
            report.superseded.push((retired, holder));
        }
    }
    Ok(())
}

/// The files the work touched, from the top of `checkout`: those its
/// branch has changed since it left the default one, committed or not, and
/// those `record` says were edited or written that are there.
fn touched(checkout: &Path, record: &Record) -> Vec<String> {
    let mut touched = crate::git::branch_changes(checkout).unwrap_or_default();
    for file in record.edited() {
        if let Some(file) = file_in(checkout, file)
            && !touched.contains(&file)
        {
            touched.push(file);
        }
    }
    touched
}

/// What the project's memory has already that the model is shown, so it
/// doesn't give it again: the entries with most to do with what the work
/// was about, and with the models, those nearest in meaning to what the
/// session said, where what it gives comes from, the two merged. A session
/// with no task has nothing it was about but what it said.
fn known(
    store: &mut Store,
    job: &Job,
    record: &Record,
    embedder: Option<&dyn embed::Embed>,
) -> Result<Vec<memory::Entry>> {
    let Some(embedder) = embedder else {
        return store.search(&job.project, &job.about, None, KNOWN_SHOWN, None);
    };
    let about = if job.about.trim().is_empty() {
        Vec::new()
    } else {
        store.search(&job.project, &job.about, None, KNOWN_SHOWN, Some(embedder))?
    };
    let said = record.said(SAID_LOOKED_AT);
    match store.nearest(&job.project, &said, KNOWN_SHOWN, embedder) {
        Ok(nearest) => Ok(memory::fused(&[nearest, about], KNOWN_SHOWN)),
        Err(err) => {
            errln!("crystal: couldn't find what the memory has near what was said: {err:#}");
            Ok(about)
        }
    }
}

/// The message a pass sends: what the work was and how it ended, what the
/// project's memory has already that has to do with it, `known`, the
/// entries gone stale it's asked about, `stale`, then what was done.
/// Credentials are taken out of all of it, so the model never sees a key a
/// session printed, to copy it into an entry.
pub fn message(header: &str, known: &[String], stale: &[Listed], record: &str) -> String {
    let mut message = secrets::redact(header);
    message.push_str("\n\nWhat the project's memory has already");
    if known.is_empty() {
        message.push_str(": nothing on this yet.");
    } else {
        message.push_str(
            ", each by its id and kind (never give any of it again, even in other words):\n",
        );
        for entry in known {
            message.push_str(&format!("- {}\n", secrets::redact(entry)));
        }
    }
    if !stale.is_empty() {
        message.push_str(
            "\n\nEntries about the files this work touched that may no longer hold (give a \
             verdict in rechecked on each the record settles, by its id):\n",
        );
        for item in stale {
            let entry = &item.entry;
            let gone = if entry.names.is_empty() {
                "every file it's about".to_string()
            } else {
                item.gone.join(", ")
            };
            message.push_str(&format!(
                "- id {}: ({}) {} [gone from the code: {gone}]\n",
                entry.id,
                entry.kind,
                secrets::redact(&memory::one_line(&entry.text)),
            ));
        }
    }
    message.push_str(&format!(
        "\n\nThe record (oldest first; the start may be cut):\n\n{}\nReturn the entries a \
         later session in this project would need.",
        secrets::redact(record),
    ));
    message
}

/// Runs Claude on `message` for `job`, and gives back its answer, the
/// structured output, and what it cost.
fn ask_claude(job: &Job, message: &str) -> Result<(Value, f64)> {
    ask(&args(&job.settings), &job.checkout, &job.env, message)
}

/// [`ask_within`] [`TIMEOUT`].
fn ask(
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    message: &str,
) -> Result<(Value, f64)> {
    ask_within(args, cwd, env, message, TIMEOUT)
}

/// Runs Claude with `args` in `cwd`, with `env`, on `message`, and gives
/// back its answer and what it cost.
fn ask_within(
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    message: &str,
    timeout: Duration,
) -> Result<(Value, f64)> {
    let mut env = env.clone();
    // It isn't a session, and its hooks are off anyway.
    for key in ["CRYSTAL_SESSION", "CRYSTAL_SESSION_ID"] {
        env.remove(key);
    }
    let mut child = Command::new("claude")
        .args(args)
        .current_dir(cwd)
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
    let _helper = HELPERS.started(pid);
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
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            signal_group(pid, libc::SIGKILL);
            let _ = child.wait();
            bail!(
                "it took longer than {}s, and was stopped",
                timeout.as_secs()
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

/// The model's entries that passed, those of them that replace an entry
/// it was shown apart, what it said of the stale entries that passed, the
/// notes it was shown that it says are lessons, with their kinds, and why
/// each that didn't pass, didn't.
#[derive(Debug, Default, PartialEq)]
pub struct Checked {
    pub entries: Vec<New>,
    pub replacements: Vec<Replacement>,
    pub rechecks: Vec<Recheck>,
    pub lessons: Vec<(u64, Kind)>,
    pub rejected: Vec<String>,
}

/// An entry the model keeps in place of one it was shown, which no longer
/// holds: `id`, replaced as `how` says, for `why`.
#[derive(Debug, Clone, PartialEq)]
pub struct Replacement {
    pub id: u64,
    pub how: How,
    pub why: String,
    pub entry: New,
}

/// How an entry that no longer holds is replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// What replaces it is it corrected: put in its place, under its id.
    Update,
    /// What replaces it is another entry: it's retired.
    Retire,
}

/// What the model said of a stale entry it was shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Recheck {
    pub id: u64,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// It still holds as it says: anchored again.
    Keep,
    /// It holds said this way instead.
    Reword(String),
    /// It no longer holds.
    Forget,
}

/// Checks the model's answer against the checkout: an array of entries,
/// bare or as `entries`, at most [`MAX_DISTILLED`]; each of a kind the
/// distiller may give, with a text that isn't empty or too long, naming
/// files that are in `checkout`; and one that replaces an entry, one of
/// `replaceable`, those it was shown, each once, saying why, and naming
/// something that's in `checkout`, when it names anything. An entry that
/// fails any of it is dropped whole, with why. Then its verdicts on the
/// stale entries, `rechecked`: each on one of `stale`, once, none replaced,
/// and one kept or reworded naming something that's in `checkout`, when it
/// names anything. Its `kinds` are kept for those of `notes`, the notes it
/// was shown, alone, as [`lessons_in`] keeps them.
pub fn check(
    answer: &Value,
    checkout: &Path,
    stale: &[Listed],
    notes: &[u64],
    replaceable: &[u64],
) -> Result<Checked> {
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
        let parsed = entry(item, checkout).and_then(|entry| {
            let replacement = replacement(&item["replaces"], &entry, checkout, replaceable)?;
            Ok((entry, replacement))
        });
        let replaced = |id: u64| checked.replacements.iter().any(|done| done.id == id);
        match parsed {
            Ok((_, Some(replacement))) if replaced(replacement.id) => {
                checked.rejected.push(format!(
                    "entry {number}: it replaces {}, as another does",
                    replacement.id
                ))
            }
            Ok((_, Some(replacement))) => checked.replacements.push(replacement),
            Ok((entry, None)) => checked.entries.push(entry),
            Err(why) => checked.rejected.push(format!("entry {number}: {why}")),
        }
    }
    let rechecked = match &answer["rechecked"] {
        Value::Array(rechecked) => &rechecked[..],
        _ => &[],
    };
    for item in rechecked {
        let id = item["id"].as_u64();
        let said = || match id {
            Some(id) => format!("entry {id}"),
            None => "a verdict".to_string(),
        };
        match recheck(item, checkout, stale) {
            Ok(recheck) if checked.rechecks.iter().any(|done| done.id == recheck.id) => {
                checked
                    .rejected
                    .push(format!("{}: a second verdict", said()));
            }
            Ok(recheck) if checked.replacements.iter().any(|r| r.id == recheck.id) => {
                checked
                    .rejected
                    .push(format!("{}: an entry replaces it", said()));
            }
            Ok(recheck) => checked.rechecks.push(recheck),
            Err(why) => checked.rejected.push(format!("{}: {why}", said())),
        }
    }
    checked.lessons = lessons_in(&answer["kinds"], notes, &mut checked.rejected);
    Ok(checked)
}

/// What `replaces`, as the model gave it with `entry`, says `entry`
/// replaces, checked: nothing, when it's null or isn't there; or one of
/// `replaceable`, how, and why, with `entry` naming something that's in
/// `checkout` when it names anything.
fn replacement(
    replaces: &Value,
    entry: &New,
    checkout: &Path,
    replaceable: &[u64],
) -> Result<Option<Replacement>, String> {
    if replaces.is_null() {
        return Ok(None);
    }
    let id = replaces["id"].as_u64().ok_or("it replaces no entry")?;
    if !replaceable.contains(&id) {
        return Err(format!("it replaces {id}, which it wasn't shown"));
    }
    let how = match replaces["how"].as_str().unwrap_or_default() {
        "update" => How::Update,
        "retire" => How::Retire,
        how => return Err(format!("{how:?} isn't how an entry is replaced")),
    };
    let why = replaces["why"].as_str().unwrap_or_default().trim();
    if why.is_empty() {
        return Err(format!("it doesn't say why {id} no longer holds"));
    }
    if why.chars().count() > MAX_WHY {
        return Err(format!(
            "why {id} no longer holds is longer than {MAX_WHY} characters"
        ));
    }
    names_what_is_there(&entry.text, &entry.files, checkout)?;
    Ok(Some(Replacement {
        id,
        how,
        why: why.to_string(),
        entry: entry.clone(),
    }))
}

/// Whether `text`, about `files`, names something that's in `checkout`,
/// when it names anything: what it says holds now, so it has to.
fn names_what_is_there(text: &str, files: &[String], checkout: &Path) -> Result<(), String> {
    let named = !memory::names_beside(text, files).is_empty();
    if named && memory::found_in(checkout, text, files).is_empty() {
        return Err("nothing it names is in the checkout".to_string());
    }
    Ok(())
}

/// A verdict on one of the `stale` entries, checked.
fn recheck(item: &Value, checkout: &Path, stale: &[Listed]) -> Result<Recheck, String> {
    let id = item["id"].as_u64().ok_or("it says of no entry")?;
    let asked = (stale.iter())
        .find(|item| item.entry.id == id)
        .ok_or("it wasn't one of those asked about")?;
    let text = item["text"].as_str().unwrap_or_default().trim();
    let verdict = match item["verdict"].as_str().unwrap_or_default() {
        "keep" => Verdict::Keep,
        "reword" => {
            text_of(text)?;
            Verdict::Reword(text.to_string())
        }
        "forget" => Verdict::Forget,
        verdict => return Err(format!("{verdict:?} isn't a verdict")),
    };
    let said = match &verdict {
        Verdict::Keep => &asked.entry.text,
        Verdict::Reword(text) => text,
        Verdict::Forget => return Ok(Recheck { id, verdict }),
    };
    names_what_is_there(said, &asked.entry.files, checkout)?;
    Ok(Recheck { id, verdict })
}

/// Whether `text` is fit for an entry: something, and not too long.
fn text_of(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("it says nothing".to_string());
    }
    if text.chars().count() > MAX_TEXT {
        return Err(format!("it's longer than {MAX_TEXT} characters"));
    }
    Ok(())
}

/// The notes `given`, an answer's list of ids with kinds, says are lessons:
/// only those of `notes`, the notes the model was shown, each once, and a
/// kind a lesson has; why each other isn't kept goes into `rejected`.
fn lessons_in(given: &Value, notes: &[u64], rejected: &mut Vec<String>) -> Vec<(u64, Kind)> {
    let given = given.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut lessons: Vec<(u64, Kind)> = Vec::new();
    for item in given {
        let Some(id) = item["id"].as_u64() else {
            rejected.push(format!("a note made a lesson with no id: {item}"));
            continue;
        };
        let kind = item["kind"].as_str().unwrap_or_default();
        let Some(kind) = Kind::parse(kind).filter(|kind| LESSONS.contains(kind)) else {
            rejected.push(format!("note {id}: {kind:?} isn't a lesson's kind"));
            continue;
        };
        if !notes.contains(&id) {
            rejected.push(format!("note {id}: not a note it was shown"));
        } else if !lessons.iter().any(|(seen, _)| *seen == id) {
            lessons.push((id, kind));
        }
    }
    lessons
}

/// What a pass over notes alone came to: the notes it says are lessons,
/// with their kinds, why each it gave that didn't pass didn't, and what it
/// cost.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Lessons {
    pub lessons: Vec<(u64, Kind)>,
    pub rejected: Vec<String>,
    pub cost_usd: f64,
}

/// Which of `notes` are lessons kept as notes, and the kind each should
/// have, as the distiller's model reads them, [`NOTES_AT_ONCE`] at a time:
/// one locked-down `claude -p` a batch, as a pass is, run in `cwd` with
/// `env`. It changes nothing: what's done with the answer is the caller's.
pub fn lessons_among(
    notes: &[Entry],
    settings: &MemorySettings,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Lessons> {
    let args = pass_args(settings, NOTES_PROMPT, &notes_schema());
    let mut found = Lessons::default();
    for batch in notes.chunks(NOTES_AT_ONCE) {
        let (answer, cost) = ask(&args, cwd, env, &notes_message(batch))?;
        found.cost_usd += cost;
        let ids: Vec<u64> = batch.iter().map(|note| note.id).collect();
        let lessons = lessons_in(&answer["lessons"], &ids, &mut found.rejected);
        found.lessons.extend(lessons);
    }
    Ok(found)
}

/// The message a pass over notes alone sends: each note on a line, after
/// its id, with the files it's about. Credentials are taken out first.
pub fn notes_message(notes: &[Entry]) -> String {
    let mut message = String::from("The notes, each after its id:\n");
    for note in notes {
        let text = memory::one_line(&note.text);
        message.push_str(&format!("- {}: {}", note.id, secrets::redact(&text)));
        if !note.files.is_empty() {
            message.push_str(&format!(" [{}]", note.files.join(", ")));
        }
        message.push('\n');
    }
    message.push_str("\nReturn the notes that are lessons, each with its kind.");
    message
}

/// An entry the model says no longer holds, as `crystal memory reconcile`
/// proposes it: `id`, which said `was` as it was read, changed as `change`
/// says, for `why`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    pub id: u64,
    pub was: String,
    pub change: Change,
    pub why: String,
}

/// What becomes of an entry that no longer holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    /// It's retired: entry `by` of its group holds in its place.
    Retire { by: u64 },
    /// It's corrected: `text` is put in its place, under its id.
    Update { text: String },
}

/// What a pass over groups of entries near one another came to: the
/// entries it says no longer hold, why each it gave that didn't pass
/// didn't, and what it cost.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Proposals {
    pub proposals: Vec<Proposal>,
    pub rejected: Vec<String>,
    pub cost_usd: f64,
}

/// Which of `groups`, entries near one another in meaning, each with
/// whether it holds, no longer hold because another of their group shows
/// it, as the distiller's model reads them: groups whole, as many as fit in
/// [`NEAR_AT_ONCE`] entries at a time, one locked-down `claude -p` each, as
/// a pass is, [`NEAR_PASSES_AT_ONCE`] at once, run in `cwd` with `env`; what
/// it gives checked against them and `cwd`, as [`proposals_in`] checks it.
/// A pass that fails says so among what's rejected, unless every one does.
/// It changes nothing: what's done with the answer is the caller's.
pub fn superseded_among(
    groups: &[Vec<Listed>],
    settings: &MemorySettings,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Proposals> {
    let args = pass_args(settings, RECONCILE_PROMPT, &reconcile_schema());
    let now = memory::seconds_since_epoch(std::time::SystemTime::now());
    let batches = batches(groups, NEAR_AT_ONCE);
    let mut answers = Vec::new();
    for run in batches.chunks(NEAR_PASSES_AT_ONCE) {
        answers.extend(thread::scope(|scope| {
            let passes: Vec<_> = (run.iter())
                .map(|batch| {
                    let message = near_message(batch, now);
                    let args = &args;
                    scope.spawn(move || ask_within(args, cwd, env, &message, NEAR_TIMEOUT))
                })
                .collect();
            let answers = passes.into_iter().map(|pass| pass.join());
            answers
                .map(|answer| answer.unwrap_or_else(|_| bail!("its pass panicked")))
                .collect::<Vec<_>>()
        }));
    }
    let mut found = Proposals::default();
    let mut failed = Vec::new();
    for (batch, answer) in batches.iter().zip(answers) {
        match answer {
            Ok((answer, cost)) => {
                found.cost_usd += cost;
                let (proposals, rejected) = proposals_in(&answer, batch, cwd);
                found.proposals.extend(proposals);
                found.rejected.extend(rejected);
            }
            Err(err) => failed.push(err),
        }
    }
    if !batches.is_empty() && failed.len() == batches.len() {
        return Err(failed.remove(0));
    }
    for err in failed {
        found.rejected.push(format!("a pass: {err:#}"));
    }
    settle(&mut found.proposals, &mut found.rejected);
    Ok(found)
}

/// `groups` in runs of whole groups, each run as many as fit in `most`
/// entries, or one group alone that doesn't.
fn batches<T>(groups: &[Vec<T>], most: usize) -> Vec<&[Vec<T>]> {
    let mut batches = Vec::new();
    let (mut start, mut entries) = (0, 0);
    for (at, group) in groups.iter().enumerate() {
        if at > start && entries + group.len() > most {
            batches.push(&groups[start..at]);
            (start, entries) = (at, 0);
        }
        entries += group.len();
    }
    if start < groups.len() {
        batches.push(&groups[start..]);
    }
    batches
}

/// The message a pass over groups of entries near one another sends: each
/// group under its number, each entry on a line after its id, with its
/// kind, how long ago it was last said at `now`, what of it is gone from
/// the code and the files it's about. Credentials are taken out first.
pub fn near_message(groups: &[Vec<Listed>], now: u64) -> String {
    let mut message = String::from(
        "Groups of entries near one another in meaning, each entry after its id (a higher id was \
         first said later):\n",
    );
    for (at, group) in groups.iter().enumerate() {
        message.push_str(&format!("\nGroup {}:\n", at + 1));
        for item in group {
            let entry = &item.entry;
            let ago = crate::tui::sidebar::ago(entry.last_seen, now);
            let mut about = format!("{}, said {ago} ago", entry.kind);
            if let Some(holds) = item.how_it_holds() {
                about.push_str(&format!("; {holds}"));
            }
            let text = memory::one_line(&entry.text);
            message.push_str(&format!(
                "- {} ({about}) {}",
                entry.id,
                secrets::redact(&text)
            ));
            if !entry.files.is_empty() {
                message.push_str(&format!(" [{}]", entry.files.join(", ")));
            }
            message.push('\n');
        }
    }
    message.push_str("\nReturn the entries that no longer hold, each with how and why.");
    message
}

/// The entries `answer` says no longer hold that pass, each as a
/// [`Proposal`], and why each that doesn't, doesn't: one of `groups`' each
/// once, saying why; retired for another of its group, `by`, which isn't
/// retired itself; or updated, with a text fit for an entry, naming
/// something that's in `checkout` when it names anything.
fn proposals_in(
    answer: &Value,
    groups: &[Vec<Listed>],
    checkout: &Path,
) -> (Vec<Proposal>, Vec<String>) {
    let given = answer["superseded"].as_array().map(Vec::as_slice);
    let mut proposals: Vec<Proposal> = Vec::new();
    let mut rejected = Vec::new();
    let group_of =
        |id: u64| (groups.iter()).find(|group| group.iter().any(|item| item.entry.id == id));
    for item in given.unwrap_or_default() {
        let Some(id) = item["id"].as_u64() else {
            rejected.push(format!("one that says of no entry: {item}"));
            continue;
        };
        let proposal = (|| -> Result<Proposal, String> {
            let group = group_of(id).ok_or("it wasn't one of those asked about")?;
            if proposals.iter().any(|done| done.id == id) {
                return Err("a second time".to_string());
            }
            let entry = &(group.iter().find(|item| item.entry.id == id))
                .expect("its group has it")
                .entry;
            let why = item["why"].as_str().unwrap_or_default().trim();
            if why.is_empty() {
                return Err("it doesn't say why it no longer holds".to_string());
            }
            if why.chars().count() > MAX_WHY {
                return Err(format!("why is longer than {MAX_WHY} characters"));
            }
            let change = match item["how"].as_str().unwrap_or_default() {
                "retire" => {
                    let by = item["by"].as_u64().ok_or("it's retired for no entry")?;
                    if by == id || !group.iter().any(|item| item.entry.id == by) {
                        return Err(format!("{by} isn't another entry of its group"));
                    }
                    Change::Retire { by }
                }
                "update" => {
                    let text = item["text"].as_str().unwrap_or_default().trim();
                    text_of(text)?;
                    if memory::one_line(text) == memory::one_line(&entry.text) {
                        return Err("its update says what it says".to_string());
                    }
                    names_what_is_there(text, &entry.files, checkout)?;
                    Change::Update {
                        text: text.to_string(),
                    }
                }
                how => return Err(format!("{how:?} isn't how an entry is replaced")),
            };
            Ok(Proposal {
                id,
                was: entry.text.clone(),
                change,
                why: why.to_string(),
            })
        })();
        match proposal {
            Ok(proposal) => proposals.push(proposal),
            Err(why) => rejected.push(format!("entry {id}: {why}")),
        }
    }
    settle(&mut proposals, &mut rejected);
    (proposals, rejected)
}

/// Leaves of `proposals` the first for each entry, and those retired for
/// one that isn't retired itself, as what holds in another's place has to
/// hold; why each other goes goes into `rejected`. An entry in two groups
/// may be proposed in two passes.
fn settle(proposals: &mut Vec<Proposal>, rejected: &mut Vec<String>) {
    let mut seen = Vec::new();
    proposals.retain(|proposal| {
        let first = !seen.contains(&proposal.id);
        if first {
            seen.push(proposal.id);
        } else {
            rejected.push(format!("entry {}: a second time", proposal.id));
        }
        first
    });
    let retired: Vec<u64> = (proposals.iter())
        .filter(|proposal| matches!(proposal.change, Change::Retire { .. }))
        .map(|proposal| proposal.id)
        .collect();
    proposals.retain(|proposal| match proposal.change {
        Change::Retire { by } if retired.contains(&by) => {
            let id = proposal.id;
            rejected.push(format!("entry {id}: {by}, in its place, is retired too"));
            false
        }
        _ => true,
    });
}

fn entry(item: &Value, checkout: &Path) -> Result<New, String> {
    let kind = item["kind"].as_str().unwrap_or_default();
    let kind = Kind::parse(kind)
        .filter(|kind| KINDS.contains(kind))
        .ok_or_else(|| format!("{kind:?} isn't a kind it may give"))?;
    let text = item["text"].as_str().unwrap_or_default().trim();
    text_of(text)?;
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
        assert_eq!(schema["properties"]["rechecked"]["maxItems"], RECHECKED);
        assert!(SYSTEM_PROMPT.contains("never guess"));
    }

    #[test]
    fn the_pass_is_told_to_keep_lessons_and_never_status() {
        for rule in [
            "the git log",
            "the project's backlog",
            "merged, pushed, committed, installed or released",
            "a commit hash",
            "only true today",
            "unless the record shows the user saying it",
            "give no line numbers",
        ] {
            assert!(SYSTEM_PROMPT.contains(rule), "{rule}");
        }
        // What it's shown as never worth keeping is what `crystal memory
        // list --status` lists.
        for status in [
            "PR #56 squash-merged into master as 8012b6c",
            "PRs #99-#104 all merged but the binary is not yet installed; run make install",
            "Backlog #81 tracks showing removal progress to other clients",
            "Flaky plugin test archived to backlog #135 during the merge",
        ] {
            assert!(SYSTEM_PROMPT.contains(&format!("\"{status}\"")), "{status}");
            assert!(memory::reads_as_status(status), "{status}");
        }
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
    fn what_the_session_said_is_its_last_words_alone() {
        let mut record = Record::default();
        for line in [
            "USER: fix the ledger",
            "ASSISTANT: looking",
            "TOOL Bash: cargo test",
            "RESULT: ok",
            "ASSISTANT:  ",
            "ASSISTANT: the ledger needs redis",
            "ASSISTANT: done",
        ] {
            record.push(line.to_string());
        }
        assert_eq!(record.said(2), ["the ledger needs redis", "done"]);
        assert_eq!(record.said(9).len(), 3);
        assert!(Record::default().said(4).is_empty());
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
            artifacts: Vec::new(),
            brief: Default::default(),
        };
        let header = header(&task);
        assert_eq!(
            header,
            "The task: fix the ledger\nHow it ended: done: redis has to be up\n\
             The project: app, on branch fix/ledger"
        );
        let known = ["3 (gotcha) redis has to be up".to_string()];
        let record = "TOOL Bash: REDIS_PASSWORD=hunter22x make\n";
        let message = message(&header, &known, &[], record);
        assert!(
            message.contains(
                "What the project's memory has already, each by its id and kind (never give any \
                 of it again, even in other words):\n- 3 (gotcha) redis has to be up\n"
            ),
            "{message}"
        );
        assert!(message.contains("REDIS_PASSWORD=[redacted]"), "{message}");
        assert!(!message.contains("hunter22x"));
        let message = super::message(&header, &[], &[], record);
        assert!(
            message.contains("has already: nothing on this yet."),
            "{message}"
        );
        assert!(!message.contains("may no longer hold"), "{message}");
    }

    /// Entry `id`, gone stale, with what of it is gone.
    fn stale(id: u64, text: &str, gone: &[&str]) -> Listed {
        let names = gone.iter().map(|name| name.to_string()).collect();
        Listed {
            entry: Entry {
                id,
                kind: Kind::Gotcha,
                text: text.into(),
                files: vec!["src/ledger.rs".into()],
                source: Source::User,
                created: 0,
                seen: 1,
                last_seen: 0,
                anchors: Default::default(),
                checkout: None,
                used: None,
                names,
                counted_from: None,
            },
            freshness: memory::Freshness::Stale,
            gone: gone.iter().map(|name| name.to_string()).collect(),
        }
    }

    #[test]
    fn the_message_asks_about_the_stale_entries_by_their_ids() {
        let stale = [
            stale(7, "Use `ledger_retry` for\nflaky calls", &["ledger_retry"]),
            Listed {
                gone: vec!["src/fees.rs".into()],
                ..stale(9, "Fees are rounded down", &[])
            },
        ];
        let message = message("The task: fix the ledger", &[], &stale, "USER: fix it\n");
        assert!(
            message.contains(
                "may no longer hold (give a verdict in rechecked on each the record settles, by \
                 its id):\n\
                 - id 7: (gotcha) Use `ledger_retry` for flaky calls [gone from the code: \
                 ledger_retry]\n\
                 - id 9: (gotcha) Fees are rounded down [gone from the code: every file it's \
                 about]\n"
            ),
            "{message}"
        );
        assert!(message.find("may no longer hold") < message.find("The record"));
    }

    #[test]
    fn verdicts_on_the_stale_entries_are_checked_against_the_checkout() {
        let dir = checkout();
        std::fs::write(
            dir.path().join("src/ledger.rs"),
            "fn ledger_retry_twice() {}",
        )
        .unwrap();
        let asked = [
            stale(7, "Use `ledger_retry` for flaky calls", &["ledger_retry"]),
            stale(8, "Call `ledger_sync` first", &["ledger_sync"]),
            stale(9, "Ledger calls are slow", &[]),
        ];
        let answer = json!({"entries": [], "rechecked": [
            {"id": 7, "verdict": "reword", "text": "Use `ledger_retry_twice` for flaky calls"},
            {"id": 8, "verdict": "keep", "text": ""},
            {"id": 9, "verdict": "keep", "text": ""},
            {"id": 7, "verdict": "forget", "text": ""},
            {"id": 3, "verdict": "forget", "text": ""},
            {"id": 8, "verdict": "doubt", "text": ""},
            {"verdict": "forget", "text": ""},
        ]});
        let checked = check(&answer, dir.path(), &asked, &[], &[]).unwrap();
        assert_eq!(
            checked.rechecks,
            [
                Recheck {
                    id: 7,
                    verdict: Verdict::Reword("Use `ledger_retry_twice` for flaky calls".into()),
                },
                Recheck {
                    id: 9,
                    verdict: Verdict::Keep,
                },
            ]
        );
        assert_eq!(
            checked.rejected,
            [
                "entry 8: nothing it names is in the checkout",
                "entry 7: a second verdict",
                "entry 3: it wasn't one of those asked about",
                "entry 8: \"doubt\" isn't a verdict",
                "a verdict: it says of no entry",
            ]
        );
        let reworded = json!({"entries": [], "rechecked": [
            {"id": 7, "verdict": "reword", "text": "Use `ledger_retries` instead"},
            {"id": 8, "verdict": "reword", "text": " "},
            {"id": 9, "verdict": "forget", "text": ""},
        ]});
        let checked = check(&reworded, dir.path(), &asked, &[], &[]).unwrap();
        assert_eq!(
            checked.rechecks,
            [Recheck {
                id: 9,
                verdict: Verdict::Forget,
            }]
        );
        assert_eq!(
            checked.rejected,
            [
                "entry 7: nothing it names is in the checkout",
                "entry 8: it says nothing",
            ]
        );
        // An answer from before has none.
        let before = json!({"entries": []});
        assert!(
            check(&before, dir.path(), &asked, &[], &[])
                .unwrap()
                .rechecks
                .is_empty()
        );
    }

    #[test]
    fn the_files_the_work_touched_include_those_it_edited() {
        let dir = checkout();
        let mut record = Record::default();
        let ledger = dir.path().join("src/ledger.rs");
        for line in [
            format!("TOOL Edit: {}", ledger.display()),
            "TOOL Write: src/ledger.rs".to_string(),
            "TOOL Read: src/other.rs".to_string(),
            "TOOL Edit: src/gone.rs".to_string(),
            "RESULT: TOOL Edit: src/x.rs".to_string(),
        ] {
            record.push(line);
        }
        assert_eq!(
            record.edited(),
            [ledger.to_str().unwrap(), "src/ledger.rs", "src/gone.rs"]
        );
        // Outside git, only what it edited that's there.
        assert_eq!(touched(dir.path(), &record), ["src/ledger.rs"]);
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
        let checked = check(&answer, dir.path(), &[], &[], &[]).unwrap();
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
        let checked = check(&Value::Array(entries), dir.path(), &[], &[], &[]).unwrap();
        assert_eq!(checked.entries.len(), MAX_DISTILLED);
        assert_eq!(checked.rejected.len(), 2);
        assert!(check(&json!("nothing"), dir.path(), &[], &[], &[]).is_err());
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
            ..Report::default()
        };
        assert_eq!(
            report.line(),
            "2 entries added, 1 seen again, 1 forgotten before, 1 rejected ($0.0123)"
        );
        let rechecked = Report {
            kept: vec![2],
            forgot: vec![3, 6],
            ..Report::default()
        };
        assert_eq!(
            rechecked.line(),
            "0 entries added, of the stale 1 kept, 2 forgotten ($0.0000)"
        );
        let made = Report {
            made_lessons: vec![7],
            ..Report::default()
        };
        assert_eq!(
            made.line(),
            "0 entries added, 1 note made a lesson ($0.0000)"
        );
    }

    #[test]
    fn notes_it_was_shown_that_are_lessons_are_made_lessons_and_nothing_else() {
        let dir = checkout();
        let answer = json!({"entries": [], "rechecked": [], "kinds": [
            {"id": 3, "kind": "gotcha"},
            {"id": 4, "kind": "decision"},
            {"id": 5, "kind": "note"},
            {"id": 3, "kind": "command"},
            {"kind": "gotcha"},
        ]});
        let checked = check(&answer, dir.path(), &[], &[3, 5], &[]).unwrap();
        assert_eq!(
            checked.lessons,
            [(3, Kind::Gotcha)],
            "the first word on each"
        );
        assert_eq!(
            checked.rejected,
            [
                "note 4: not a note it was shown",
                "note 5: \"note\" isn't a lesson's kind",
                "a note made a lesson with no id: {\"kind\":\"gotcha\"}",
            ]
        );
        let schema = schema();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("kinds"))
        );
        let kinds = &schema["properties"]["kinds"]["items"]["properties"]["kind"]["enum"];
        assert_eq!(*kinds, json!(["decision", "gotcha", "command"]));
        assert!(SYSTEM_PROMPT.contains("give its id in kinds"));
    }

    #[test]
    fn a_pass_over_notes_reads_each_by_its_id_and_is_told_what_is_no_lesson() {
        let note = |id, text: &str, files: &[&str]| Entry {
            id,
            kind: Kind::Note,
            text: text.into(),
            files: files.iter().map(|file| file.to_string()).collect(),
            source: Source::User,
            created: 0,
            seen: 1,
            last_seen: 0,
            anchors: Default::default(),
            checkout: None,
            used: None,
            names: Vec::new(),
            counted_from: None,
        };
        let notes = [
            note(
                3,
                "The ledger tests need\nREDIS_PASSWORD=hunter22x set",
                &["tests/ledger.rs"],
            ),
            note(5, "The sidebar's width is kept in the ui table", &[]),
        ];
        assert_eq!(
            notes_message(&notes),
            "The notes, each after its id:\n\
             - 3: The ledger tests need REDIS_PASSWORD=[redacted] set [tests/ledger.rs]\n\
             - 5: The sidebar's width is kept in the ui table\n\
             \nReturn the notes that are lessons, each with its kind."
        );
        for rule in [
            "a decision: a choice made and why",
            "a gotcha: a trap or a surprise",
            "a command: a command line to run",
            "These are not lessons, and stay notes",
            "When unsure, leave it out",
        ] {
            assert!(NOTES_PROMPT.contains(rule), "{rule}");
        }
        let schema = notes_schema();
        assert_eq!(schema["required"], json!(["lessons"]));
        assert_eq!(schema["properties"]["lessons"]["maxItems"], NOTES_AT_ONCE);
        let args = pass_args(&MemorySettings::default(), NOTES_PROMPT, &schema);
        assert!(args.contains(&"--no-session-persistence".to_string()));
        assert!(args.contains(&NOTES_PROMPT.to_string()));
    }

    #[test]
    fn what_replaces_an_entry_is_checked_against_what_it_was_shown_and_the_checkout() {
        let dir = checkout();
        std::fs::write(
            dir.path().join("src/ledger.rs"),
            "fn ledger_retry_twice() {}",
        )
        .unwrap();
        let replaces = |id: u64, how: &str, why: &str| json!({"id": id, "how": how, "why": why});
        let entry = |text: &str, replaces: Value| {
            let files: [&str; 0] = [];
            json!({"kind": "gotcha", "text": text, "files": files, "replaces": replaces})
        };
        let answer = json!({"entries": [
            entry("Retry with `ledger_retry_twice`", replaces(3, "update", "it was renamed")),
            entry("Ledger calls are fast", replaces(4, "retire", "  ")),
            entry("Ledger calls time out", replaces(3, "retire", "it changed")),
            entry("Call `ledger_sync` first", replaces(4, "retire", "it changed")),
            entry("Ledger calls block", replaces(4, "merge", "it changed")),
            entry("Ledger calls are cached", Value::Null),
            json!({"kind": "note", "text": "Before replaces was asked for", "files": []}),
            entry("Ledger calls are queued", replaces(4, "retire", "the queue replaced them")),
        ], "rechecked": [
            {"id": 3, "verdict": "forget", "text": ""},
        ]});
        let stale = [stale(3, "Retry with `ledger_retry`", &["ledger_retry"])];
        let checked = check(&answer, dir.path(), &stale, &[], &[3, 4]).unwrap();
        let replaced: Vec<(u64, How, &str, &str)> = (checked.replacements.iter())
            .map(|r| (r.id, r.how, r.why.as_str(), r.entry.text.as_str()))
            .collect();
        assert_eq!(
            replaced,
            [
                (
                    3,
                    How::Update,
                    "it was renamed",
                    "Retry with `ledger_retry_twice`"
                ),
                (
                    4,
                    How::Retire,
                    "the queue replaced them",
                    "Ledger calls are queued"
                ),
            ]
        );
        let texts: Vec<&str> = checked.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Ledger calls are cached", "Before replaces was asked for"]
        );
        assert_eq!(
            checked.rejected,
            [
                "entry 2: it doesn't say why 4 no longer holds",
                "entry 3: it replaces 3, as another does",
                "entry 4: nothing it names is in the checkout",
                "entry 5: \"merge\" isn't how an entry is replaced",
                "entry 3: an entry replaces it",
            ]
        );
        assert!(checked.rechecks.is_empty());
        let unshown = json!({"entries": [entry("Calls are slow", replaces(9, "retire", "why"))]});
        let checked = check(&unshown, dir.path(), &[], &[], &[3, 4]).unwrap();
        assert_eq!(
            checked.rejected,
            ["entry 1: it replaces 9, which it wasn't shown"]
        );
        assert!(checked.entries.is_empty() && checked.replacements.is_empty());
    }

    #[test]
    fn the_pass_is_told_how_to_replace_what_no_longer_holds() {
        let schema = schema();
        let entry = &schema["properties"]["entries"]["items"];
        assert!(
            entry["required"]
                .as_array()
                .unwrap()
                .contains(&json!("replaces"))
        );
        let replaces = &entry["properties"]["replaces"]["anyOf"];
        assert_eq!(replaces[0], json!({"type": "null"}));
        assert_eq!(
            replaces[1]["properties"]["how"]["enum"],
            json!(["update", "retire"])
        );
        assert_eq!(replaces[1]["required"], json!(["id", "how", "why"]));
        for rule in [
            "say so in that entry's replaces",
            "update when yours is that entry corrected",
            "retire when yours is a new statement",
            "never replace one that still holds",
        ] {
            assert!(SYSTEM_PROMPT.contains(rule), "{rule}");
        }
    }

    /// `entries`, each `(id, text)`, as a group of entries near one another,
    /// each said `ago` seconds before `NOW`, and the last drifting.
    fn group(entries: &[(u64, &str)]) -> Vec<Listed> {
        let last = entries.len() - 1;
        (entries.iter().enumerate())
            .map(|(at, &(id, text))| {
                let mut item = stale(id, text, &[]);
                item.entry.kind = Kind::Decision;
                item.entry.last_seen = NOW - 3600 * (entries.len() - at) as u64;
                item.freshness = if at == last {
                    memory::Freshness::Drifting
                } else {
                    memory::Freshness::Fresh
                };
                item.entry.names = vec!["ledger_retry".into()];
                item.gone = (at == last)
                    .then(|| "ledger_retry".to_string())
                    .into_iter()
                    .collect();
                item
            })
            .collect()
    }

    const NOW: u64 = 1_000_000;

    #[test]
    fn groups_near_one_another_are_shown_by_their_ids_with_what_is_gone() {
        let groups = [
            group(&[
                (2, "Idle stop is off\nby default"),
                (7, "Idle stop is on by default"),
            ]),
            group(&[(5, "API_KEY=sk-abcdef123456 runs the ledger")]),
        ];
        let message = near_message(&groups, NOW);
        assert!(
            message.contains(
                "Group 1:\n\
                 - 2 (decision, said 2h ago) Idle stop is off by default [src/ledger.rs]\n\
                 - 7 (decision, said 1h ago; drifting: some of what it names is gone from the \
                 code: ledger_retry) Idle stop is on by default [src/ledger.rs]\n"
            ),
            "{message}"
        );
        assert!(message.contains("Group 2:\n- 5 "), "{message}");
        assert!(!message.contains("sk-abcdef123456"), "{message}");
        assert!(RECONCILE_PROMPT.contains("When unsure, leave it out"));
        let schema = reconcile_schema();
        assert_eq!(
            schema["properties"]["superseded"]["items"]["required"],
            json!(["id", "how", "by", "text", "why"])
        );
        // Whole groups, as many as fit.
        let sizes = |groups: &[Vec<u8>], most| -> Vec<usize> {
            batches(groups, most)
                .iter()
                .map(|batch| batch.len())
                .collect()
        };
        let groups = [vec![0; 3], vec![0; 2], vec![0; 4], vec![0; 9], vec![0; 1]];
        assert_eq!(sizes(&groups, 5), [2, 1, 1, 1]);
        assert_eq!(sizes(&groups, 40), [5]);
        assert!(sizes(&[], 5).is_empty());
    }

    #[test]
    fn what_no_longer_holds_is_checked_against_its_group_and_the_checkout() {
        let dir = checkout();
        std::fs::write(
            dir.path().join("src/ledger.rs"),
            "fn ledger_retry_twice() {}",
        )
        .unwrap();
        let groups = [
            group(&[
                (2, "Idle stop is off"),
                (7, "Idle stop is on"),
                (8, "Idle stop spares"),
            ]),
            group(&[(4, "Use `ledger_retry`"), (9, "Calls are slow")]),
        ];
        let retire = |id: u64, by: Value, why: &str| {
            let text = "";
            json!({"id": id, "how": "retire", "by": by, "text": text, "why": why})
        };
        let update = |id: u64, text: &str| {
            let why = "renamed";
            json!({"id": id, "how": "update", "by": null, "text": text, "why": why})
        };
        let answer = json!({"superseded": [
            retire(2, json!(7), "the default flipped"),
            retire(9, json!(2), "another group's"),
            retire(4, json!(4), "itself"),
            retire(8, Value::Null, "none"),
            update(4, "Use `ledger_retry_twice`"),
            update(9, "Calls go through `ledger_sync`"),
            update(9, "Calls are slow"),
            retire(2, json!(8), "a second time"),
            retire(3, json!(2), "not asked"),
            retire(7, json!(8), "  "),
            {"how": "retire"},
        ]});
        let (proposals, rejected) = proposals_in(&answer, &groups, dir.path());
        assert_eq!(
            proposals,
            [
                Proposal {
                    id: 2,
                    was: "Idle stop is off".into(),
                    change: Change::Retire { by: 7 },
                    why: "the default flipped".into(),
                },
                Proposal {
                    id: 4,
                    was: "Use `ledger_retry`".into(),
                    change: Change::Update {
                        text: "Use `ledger_retry_twice`".into()
                    },
                    why: "renamed".into(),
                },
            ]
        );
        assert_eq!(
            rejected,
            [
                "entry 9: 2 isn't another entry of its group",
                "entry 4: 4 isn't another entry of its group",
                "entry 8: it's retired for no entry",
                "entry 9: nothing it names is in the checkout",
                "entry 9: its update says what it says",
                "entry 2: a second time",
                "entry 3: it wasn't one of those asked about",
                "entry 7: it doesn't say why it no longer holds",
                "one that says of no entry: {\"how\":\"retire\"}",
            ]
        );
        // What holds in another's place has to hold itself.
        let chained = json!({"superseded": [
            retire(2, json!(7), "flipped"),
            retire(7, json!(8), "flipped again"),
        ]});
        let (proposals, rejected) = proposals_in(&chained, &groups, dir.path());
        assert_eq!(proposals.iter().map(|p| p.id).collect::<Vec<_>>(), [7]);
        assert_eq!(rejected, ["entry 2: 7, in its place, is retired too"]);
    }
}
