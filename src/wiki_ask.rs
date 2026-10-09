//! The wiki's chat: a question about a project's code, asked on its wiki's
//! page, answered by Claude Code headless in the project's main worktree,
//! its answer streamed back to the page as it comes (see
//! [`crate::wiki_server`]).
//!
//! One `claude -p` a question, given the question on its standard input
//! and, appended to its system prompt, the wiki's outline (each section's
//! title and summary, and the section the user is reading in full) and the
//! rule to answer with `code:` links to the lines it read. It may only
//! read: Read, Grep, Glob, `git log` and `git ls-files`, anything else
//! refused without asking; no MCP servers, nobody's hooks, none of the
//! project's settings, and a budget a question (`[wiki]`). A follow-up
//! resumes the conversation the page was told of.
//!
//! What it writes, with `--output-format stream-json` and
//! `--include-partial-messages`, is read into what the page is sent, as
//! server-sent events: `delta`, the answer's text as it streams; `tool`, a
//! tool it uses and the file it reads; then `done`, with the conversation
//! and what it cost, or `error`, saying why it failed. A page that goes
//! away stops its `claude`.

use crate::config::{Config, WikiSettings};
use crate::printable;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The tools Claude may use, and the ones it may use without asking, which
/// are all it may: with `dontAsk`, everything else is refused.
const TOOLS: &str = "Read,Grep,Glob,Bash";
const ALLOWED: &str = "Read,Grep,Glob,Bash(git log:*),Bash(git ls-files:*)";

/// Nobody's hooks: crystal's would take it for a session.
const SETTINGS: &str = r#"{"disableAllHooks":true}"#;

/// The longest question taken, in bytes.
pub const MAX_QUESTION: usize = 8 * 1024;

/// The most of the section the user is reading that Claude is given, in
/// bytes: the rest it reads in the code.
const SECTION_CAP: usize = 24 * 1024;

/// How often the page is sent a line that says nothing while Claude
/// thinks, which finds a page that went away.
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// The longest a question may take.
const TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// What the page asks: `POST …/api/ask`'s body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Question {
    pub question: String,
    /// The conversation an earlier answer named, to follow up in.
    #[serde(default)]
    pub conversation: Option<String>,
    /// The id of the section the user is reading.
    #[serde(default)]
    pub section: Option<String>,
}

impl Question {
    /// Why the question can't be asked, if it can't.
    pub fn refused(&self) -> Option<String> {
        if self.question.trim().is_empty() {
            return Some("the question is empty".to_string());
        }
        if self.question.len() > MAX_QUESTION {
            return Some(format!(
                "the question is longer than {} KiB",
                MAX_QUESTION / 1024
            ));
        }
        if let Some(conversation) = &self.conversation
            && !is_conversation(conversation)
        {
            return Some(format!("{conversation} isn't a conversation"));
        }
        None
    }
}

/// Whether `id` looks like a conversation's id, a UUID, which is what's
/// passed to `--resume`.
fn is_conversation(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Something to tell the page: one of the contract's events.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    /// More of the answer's text.
    Delta(String),
    /// Claude used a tool: its name, the file or directory it's about, from
    /// the repository's root where it's in it, and what else it asked, like
    /// a pattern or a command.
    Tool {
        name: String,
        path: Option<String>,
        detail: Option<String>,
    },
    /// The answer is complete.
    Done { conversation: String, cost_usd: f64 },
    /// It failed, saying why.
    Error(String),
}

impl Said {
    /// The event's name and its data, as the page reads them.
    pub fn event(&self) -> (&'static str, Value) {
        match self {
            Said::Delta(text) => ("delta", json!({ "text": text })),
            Said::Tool { name, path, detail } => {
                let mut data = json!({ "name": name, "path": path });
                if let Some(detail) = detail {
                    data["detail"] = json!(detail);
                }
                ("tool", data)
            }
            Said::Done {
                conversation,
                cost_usd,
            } => (
                "done",
                json!({ "conversation": conversation, "cost_usd": cost_usd }),
            ),
            Said::Error(message) => ("error", json!({ "message": message })),
        }
    }
}

/// Writes `said` to the page as a server-sent event, and sends it on.
pub fn send(out: &mut dyn Write, said: &Said) -> io::Result<()> {
    let (event, data) = said.event();
    write!(out, "event: {event}\ndata: {data}\n\n")?;
    out.flush()
}

/// Reads Claude's stream-json, a line at a time, into what the page is
/// told.
#[derive(Debug, Default)]
pub struct Reading {
    /// The repository's root, as it was given and as it is, which tools'
    /// paths are given from.
    roots: Vec<PathBuf>,
    /// The text streams in pieces: the whole messages that follow them say
    /// it again, and are passed over.
    partial: bool,
    /// Some text has been sent: the next block of it starts a paragraph.
    spoke: bool,
    /// The budget a question has, for saying it was spent.
    budget_usd: f64,
    /// The answer has ended, done or failed.
    pub ended: bool,
}

impl Reading {
    pub fn new(root: &Path, budget_usd: f64) -> Reading {
        let mut roots = vec![root.to_path_buf()];
        roots.extend(std::fs::canonicalize(root).ok().filter(|real| real != root));
        Reading {
            roots,
            budget_usd,
            ..Reading::default()
        }
    }

    /// What `line` says for the page.
    pub fn take(&mut self, line: &str) -> Vec<Said> {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        match event["type"].as_str() {
            Some("stream_event") => self.partial_event(&event["event"]),
            Some("assistant") => self.message(&event["message"]),
            Some("result") => {
                self.ended = true;
                vec![self.result(&event)]
            }
            _ => Vec::new(),
        }
    }

    /// A piece of a message as it streams: text, or a new block of it.
    fn partial_event(&mut self, event: &Value) -> Vec<Said> {
        self.partial = true;
        match event["type"].as_str() {
            Some("content_block_start") if event["content_block"]["type"] == "text" => {
                self.paragraph().into_iter().collect()
            }
            Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                match event["delta"]["text"].as_str() {
                    Some(text) if !text.is_empty() => {
                        self.spoke = true;
                        vec![Said::Delta(text.to_string())]
                    }
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    /// A blank line between this block of text and the one before.
    fn paragraph(&mut self) -> Option<Said> {
        self.spoke.then(|| Said::Delta("\n\n".to_string()))
    }

    /// A whole message: the tools it uses, and its text when it didn't
    /// stream.
    fn message(&mut self, message: &Value) -> Vec<Said> {
        let mut said = Vec::new();
        let blocks = message["content"].as_array().map(Vec::as_slice);
        for block in blocks.unwrap_or_default() {
            match block["type"].as_str() {
                Some("text") if !self.partial => {
                    let text = block["text"].as_str().unwrap_or_default();
                    if text.is_empty() {
                        continue;
                    }
                    said.extend(self.paragraph());
                    self.spoke = true;
                    said.push(Said::Delta(text.to_string()));
                }
                Some("tool_use") => said.push(self.tool(block)),
                _ => {}
            }
        }
        said
    }

    fn tool(&self, block: &Value) -> Said {
        let input = &block["input"];
        let path = ["file_path", "path", "notebook_path"]
            .iter()
            .find_map(|key| input[key].as_str())
            .map(|path| self.below_root(path));
        let detail = ["pattern", "command"]
            .iter()
            .find_map(|key| input[key].as_str())
            .map(str::to_string);
        Said::Tool {
            name: block["name"].as_str().unwrap_or("tool").to_string(),
            path,
            detail,
        }
    }

    /// `path` from the repository's root, when it's in it.
    fn below_root(&self, path: &str) -> String {
        let path = Path::new(path);
        let below = self
            .roots
            .iter()
            .find_map(|root| path.strip_prefix(root).ok());
        match below {
            Some(below) if below.as_os_str().is_empty() => ".".to_string(),
            Some(below) => below.display().to_string(),
            None => path.display().to_string(),
        }
    }

    /// How the run ended: done, with its conversation and cost, or why it
    /// failed.
    fn result(&self, event: &Value) -> Said {
        let failed = event["is_error"].as_bool().unwrap_or(false)
            || event["subtype"]
                .as_str()
                .is_some_and(|kind| kind != "success");
        if !failed {
            return Said::Done {
                conversation: event["session_id"].as_str().unwrap_or_default().to_string(),
                cost_usd: event["total_cost_usd"].as_f64().unwrap_or(0.0),
            };
        }
        let why = match event["subtype"].as_str() {
            Some("error_max_budget_usd") => format!(
                "answering would cost more than the ${:.2} a question may spend: \
                 `[wiki] ask_budget_usd` in crystal's config says how much",
                self.budget_usd
            ),
            Some("error_max_turns") => "it took more turns than it may".to_string(),
            _ => match event["result"].as_str().map(str::trim) {
                Some(said) if !said.is_empty() => said.to_string(),
                _ => "Claude stopped without answering".to_string(),
            },
        };
        Said::Error(why)
    }
}

/// What Claude is told beside its own system prompt: what it's answering
/// about, how to link into the code, and the wiki's outline, the section
/// with the id `reading` in full.
pub fn system_prompt(wiki: &Value, reading: Option<&str>) -> String {
    let repo = &wiki["repo"];
    let name = repo["name"].as_str().unwrap_or("this project");
    let commit = repo["commit"].as_str().unwrap_or("unknown");
    let mut prompt = format!(
        "You answer questions about the code of {name}, asked by someone reading its wiki, a \
         page about its code. You are in its repository; the wiki was written at commit \
         {commit}, and the code may have moved on since. Read the code to answer: never answer \
         from the outline below alone, and never make up a line you haven't read.\n\n\
         Answer in markdown, briefly and exactly. Whenever you name a file, a type, a function \
         or a line, link it into the code: [label](code:PATH#L10) for a line, \
         [label](code:PATH#L10-L20) for lines, [label](code:PATH) for a whole file, PATH from \
         the repository's root, the lines as you read them. Link to a part of the wiki with \
         [title](#id), by the ids below.\n\nThe wiki's outline:\n"
    );
    let summary = |value: &Value| first_paragraph(value.as_str().unwrap_or_default());
    prompt.push_str(&format!(
        "- Overview: {}\n",
        summary(&wiki["overview"]["summary_md"])
    ));
    let sections = wiki["sections"].as_array().map(Vec::as_slice);
    let mut reading_in = None;
    for section in sections.unwrap_or_default() {
        let id = section["id"].as_str().unwrap_or_default();
        prompt.push_str(&format!(
            "- {} (#{id}): {}\n",
            section["title"].as_str().unwrap_or_default(),
            summary(&section["summary_md"]),
        ));
        let subsections = section["subsections"].as_array().map(Vec::as_slice);
        for subsection in subsections.unwrap_or_default() {
            let sub = subsection["id"].as_str().unwrap_or_default();
            prompt.push_str(&format!(
                "  - {} (#{sub})\n",
                subsection["title"].as_str().unwrap_or_default()
            ));
            if reading == Some(sub) {
                reading_in = Some((section, Some(subsection)));
            }
        }
        if reading == Some(id) {
            reading_in = Some((section, None));
        }
    }
    if let Some((section, subsection)) = reading_in {
        let mut text = String::new();
        text.push_str(&format!(
            "## {}\n\n{}\n\n",
            section["title"].as_str().unwrap_or_default(),
            section["summary_md"].as_str().unwrap_or_default()
        ));
        for sub in section["subsections"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            text.push_str(&format!(
                "### {}\n\n{}\n\n",
                sub["title"].as_str().unwrap_or_default(),
                sub["body_md"].as_str().unwrap_or_default()
            ));
        }
        let at = match subsection {
            Some(sub) => format!(
                "\"{}\", under \"{}\"",
                sub["title"].as_str().unwrap_or_default(),
                section["title"].as_str().unwrap_or_default()
            ),
            None => format!("\"{}\"", section["title"].as_str().unwrap_or_default()),
        };
        prompt.push_str(&format!(
            "\nThe user is reading {at}. Its section of the wiki, in full:\n\n{}",
            cut(&text, SECTION_CAP)
        ));
    }
    prompt
}

/// The first paragraph of `markdown`, on one line.
fn first_paragraph(markdown: &str) -> String {
    let paragraph = markdown.trim().split("\n\n").next().unwrap_or_default();
    paragraph.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text`, cut to at most `cap` bytes at a character's edge.
fn cut(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Claude's arguments for `question`, told `prompt` beside its own system
/// prompt, as `settings` say.
pub fn args(question: &Question, settings: &WikiSettings, prompt: &str) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--model",
        &settings.ask_model,
        "--append-system-prompt",
        prompt,
        "--tools",
        TOOLS,
        "--allowedTools",
        ALLOWED,
        "--permission-mode",
        "dontAsk",
        "--strict-mcp-config",
        "--setting-sources",
        "user",
        "--settings",
        SETTINGS,
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect();
    if settings.ask_budget_usd > 0.0 {
        args.extend([
            "--max-budget-usd".to_string(),
            settings.ask_budget_usd.to_string(),
        ]);
    }
    if let Some(conversation) = &question.conversation {
        args.extend(["--resume".to_string(), conversation.clone()]);
    }
    args
}

/// Answers `question` about the project whose wiki is `wiki`, in `root`,
/// its main worktree, writing what the page is told to `out` as it comes.
/// Fails only when the page has gone, once Claude is stopped.
pub fn answer(
    question: &Question,
    wiki: &Value,
    root: &Path,
    out: &mut dyn Write,
) -> io::Result<()> {
    if let Some(why) = question.refused() {
        return send(out, &Said::Error(why));
    }
    let settings = match Config::load() {
        Ok(config) => config.wiki,
        Err(err) => return send(out, &Said::Error(format!("{err:#}"))),
    };
    let prompt = system_prompt(wiki, question.section.as_deref());
    let mut command = Command::new("claude");
    command
        .args(args(question, &settings, &prompt))
        .current_dir(root)
        // It isn't a session.
        .env_remove("CRYSTAL_SESSION")
        .env_remove("CRYSTAL_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A process group of its own, to stop all of it.
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => return send(out, &Said::Error(format!("couldn't start claude: {err}"))),
    };
    let mut stdin = child.stdin.take().expect("its input is piped");
    let text = question.question.clone();
    thread::spawn(move || {
        // Closed once written: that's the end of the prompt.
        let _ = stdin.write_all(text.as_bytes());
    });
    let errors = read_errors(child.stderr.take().expect("its errors are piped"));
    let lines = read_lines(child.stdout.take().expect("its output is piped"));
    let mut reading = Reading::new(root, settings.ask_budget_usd);
    let deadline = Instant::now() + TIMEOUT;
    let told = (|| -> io::Result<()> {
        loop {
            let line = match lines.recv_timeout(KEEP_ALIVE) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        stop(&mut child);
                        let why = format!("it took longer than {} minutes", TIMEOUT.as_secs() / 60);
                        return send(out, &Said::Error(why));
                    }
                    // A comment, which the page passes over.
                    out.write_all(b": still thinking\n\n")?;
                    out.flush()?;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            for said in reading.take(&line) {
                send(out, &said)?;
            }
        }
        if reading.ended {
            return Ok(());
        }
        let status = child.wait()?;
        let errors = errors.join().unwrap_or_default();
        let why = match errors.last() {
            Some(line) => format!("claude failed: {line}"),
            None => format!("claude ended without answering ({status})"),
        };
        send(out, &Said::Error(why))
    })();
    if told.is_err() {
        // The page has gone: so has what it asked.
        stop(&mut child);
    }
    let _ = child.wait();
    told
}

/// Claude's output, a line at a time, on a channel that closes as the
/// output does.
fn read_lines(stdout: impl Read + Send + 'static) -> mpsc::Receiver<String> {
    let (send, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if send.send(line).is_err() {
                break;
            }
        }
    });
    lines
}

/// The last lines Claude wrote on its standard error, for saying why it
/// failed.
fn read_errors(stderr: impl Read + Send + 'static) -> thread::JoinHandle<Vec<String>> {
    thread::spawn(move || {
        let mut kept = VecDeque::new();
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            let line = printable::line(&line);
            if line.trim().is_empty() {
                continue;
            }
            if kept.len() == 5 {
                kept.pop_front();
            }
            kept.push_back(line.trim().to_string());
        }
        kept.into()
    })
}

/// Stops `child` and whatever it started: asked first, then made to.
fn stop(child: &mut Child) {
    let group = -(child.id() as i32);
    // SAFETY: kill has no preconditions; the group is the child's own.
    unsafe {
        libc::kill(group, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: as above.
    unsafe {
        libc::kill(group, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(lines: &[&str]) -> Vec<Said> {
        let mut reading = Reading::new(Path::new("/code/app"), 0.5);
        lines.iter().flat_map(|line| reading.take(line)).collect()
    }

    #[test]
    fn the_answer_streams_its_tools_and_its_end() {
        let said = read(&[
            r#"{"type":"system","subtype":"init","session_id":"0b5c-77"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","name":"Read"}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/code/app/src/main.rs"}}]}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"It starts in "}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"[main](code:src/main.rs#L3)."}}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"It starts in [main](code:src/main.rs#L3)."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Grep","input":{"pattern":"fn run","path":"/code/app/src"}}]}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"text","text":""}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Then run."}}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"0b5c-77","total_cost_usd":0.031}"#,
        ]);
        let tool = |name: &str, path: &str, detail: Option<&str>| Said::Tool {
            name: name.into(),
            path: Some(path.into()),
            detail: detail.map(String::from),
        };
        assert_eq!(
            said,
            vec![
                tool("Read", "src/main.rs", None),
                Said::Delta("It starts in ".into()),
                Said::Delta("[main](code:src/main.rs#L3).".into()),
                tool("Grep", "src", Some("fn run")),
                Said::Delta("\n\n".into()),
                Said::Delta("Then run.".into()),
                Said::Done {
                    conversation: "0b5c-77".into(),
                    cost_usd: 0.031
                },
            ]
        );
    }

    #[test]
    fn whole_messages_are_the_text_when_nothing_streamed() {
        let said = read(&[
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"One."}]}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Two."}]}}"#,
        ]);
        assert_eq!(
            said,
            vec![
                Said::Delta("One.".into()),
                Said::Delta("\n\n".into()),
                Said::Delta("Two.".into())
            ]
        );
        assert!(read(&["not json", r#"{"type":"user"}"#]).is_empty());
    }

    #[test]
    fn a_run_that_failed_says_why() {
        let failed = |line: &str| read(&[line]).pop().unwrap();
        assert_eq!(
            failed(r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true}"#),
            Said::Error(
                "answering would cost more than the $0.50 a question may spend: \
                 `[wiki] ask_budget_usd` in crystal's config says how much"
                    .into()
            )
        );
        assert_eq!(
            failed(
                r#"{"type":"result","subtype":"success","is_error":true,"result":"API Error: 529 overloaded"}"#
            ),
            Said::Error("API Error: 529 overloaded".into())
        );
        assert_eq!(
            failed(r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#),
            Said::Error("Claude stopped without answering".into())
        );
    }

    #[test]
    fn each_event_is_the_contract_s() {
        let mut out = Vec::new();
        send(&mut out, &Said::Delta("a \"b\"\nc".into())).unwrap();
        let tool = Said::Tool {
            name: "Read".into(),
            path: Some("src/x.rs".into()),
            detail: None,
        };
        send(&mut out, &tool).unwrap();
        let done = Said::Done {
            conversation: "c-1".into(),
            cost_usd: 0.03,
        };
        send(&mut out, &done).unwrap();
        send(&mut out, &Said::Error("no".into())).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "event: delta\ndata: {\"text\":\"a \\\"b\\\"\\nc\"}\n\n\
             event: tool\ndata: {\"name\":\"Read\",\"path\":\"src/x.rs\"}\n\n\
             event: done\ndata: {\"conversation\":\"c-1\",\"cost_usd\":0.03}\n\n\
             event: error\ndata: {\"message\":\"no\"}\n\n"
        );
    }

    #[test]
    fn a_question_is_refused_empty_too_long_or_in_a_conversation_that_isn_t_one() {
        let question = |text: &str, conversation: Option<&str>| Question {
            question: text.into(),
            conversation: conversation.map(String::from),
            section: None,
        };
        assert_eq!(question("How?", None).refused(), None);
        let uuid = "ebf4cee3-7c34-4945-a896-201b1a2fbfc4";
        assert_eq!(question("How?", Some(uuid)).refused(), None);
        assert!(question("  \n", None).refused().is_some());
        assert!(
            question(&"x".repeat(MAX_QUESTION + 1), None)
                .refused()
                .is_some()
        );
        assert!(
            question("How?", Some("--dangerously-skip-permissions"))
                .refused()
                .is_some()
        );
    }

    #[test]
    fn claude_may_only_read_and_follows_up_in_its_conversation() {
        let settings = WikiSettings::default();
        let question = Question {
            question: "How?".into(),
            conversation: Some("ebf4cee3-7c34".into()),
            section: None,
        };
        let args = args(&question, &settings, "PROMPT");
        let after = |flag: &str| {
            let at = args.iter().position(|arg| arg == flag).unwrap();
            args[at + 1].as_str()
        };
        assert_eq!(after("--model"), "sonnet");
        assert_eq!(after("--append-system-prompt"), "PROMPT");
        assert_eq!(after("--tools"), "Read,Grep,Glob,Bash");
        assert_eq!(
            after("--allowedTools"),
            "Read,Grep,Glob,Bash(git log:*),Bash(git ls-files:*)"
        );
        assert_eq!(after("--permission-mode"), "dontAsk");
        assert_eq!(after("--setting-sources"), "user");
        assert_eq!(after("--settings"), r#"{"disableAllHooks":true}"#);
        assert_eq!(after("--max-budget-usd"), "0.5");
        assert_eq!(after("--resume"), "ebf4cee3-7c34");
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert!(args.contains(&"--include-partial-messages".to_string()));
    }

    #[test]
    fn claude_is_told_the_outline_and_the_section_being_read_in_full() {
        let wiki = json!({
            "repo": {"name": "acme/app", "commit": "abc123"},
            "overview": {"summary_md": "An app.\n\nMore about it."},
            "sections": [
                {"id": "start", "title": "Starting", "summary_md": "How it starts.",
                 "subsections": [{"id": "main", "title": "Main", "body_md": "The main body."}]},
                {"id": "end", "title": "Ending", "summary_md": "How it ends.",
                 "subsections": [{"id": "exit", "title": "Exit", "body_md": "The exit body."}]}
            ]
        });
        let prompt = system_prompt(&wiki, Some("main"));
        assert!(prompt.contains("the code of acme/app"));
        assert!(prompt.contains("at commit abc123"));
        assert!(prompt.contains("[label](code:PATH#L10-L20)"));
        assert!(prompt.contains("- Overview: An app.\n"), "{prompt}");
        assert!(prompt.contains("- Starting (#start): How it starts.\n  - Main (#main)\n"));
        assert!(prompt.contains("- Ending (#end): How it ends.\n  - Exit (#exit)\n"));
        assert!(prompt.contains("The user is reading \"Main\", under \"Starting\""));
        assert!(prompt.contains("### Main\n\nThe main body."));
        assert!(!prompt.contains("The exit body."), "only the section read");
        let prompt = system_prompt(&wiki, None);
        assert!(!prompt.contains("The user is reading"));
        assert!(!prompt.contains("The main body."));
    }
}
