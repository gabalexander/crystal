//! Reading what `claude -p` says as it works, and writing it out as lines
//! a person reads: the prompt, what Claude says, each tool it uses with the
//! first line of what came back, the permissions it asks for and how they
//! were answered, and how the run ended.
//!
//! With `--output-format stream-json`, Claude writes one JSON event per
//! line: `system` (`init` names the conversation), `assistant` (text and
//! tool uses), `user` (tool results) and, last, `result`. A task draws these
//! on its session's screen, so the lines here carry ANSI colors and end in
//! `\r\n`, the way any program's output does on a terminal. What Claude
//! says is markdown, laid out as a page for the screen's width.
//!
//! None of what goes into them is crystal's own: Claude's text, a tool's
//! answer, a command's name, what claude wrote to its standard error. Each
//! goes through [`printable`] first, so the screen only ever draws it,
//! never takes an order from it: it can't set the session's title, write
//! the user's clipboard, make a link or switch the screen's modes.
//!
//! Each assistant message says how many tokens the model was given for it,
//! which is how full the conversation's context is, and the result says
//! how many each model takes: a task's context meter. The transcript Claude
//! Code keeps of a conversation holds the same messages, and the prompts,
//! but no results: a task's screen is drawn again from it after a restart
//! (see [`kept_events`]), made printable the same way.

use crate::markdown::{self, Ink, Mark};
use crate::printable;
use crate::syntax::TokenKind;
use ratatui::style::Modifier;
use serde_json::Value;

/// How much of a tool's input or answer a line shows.
const GIST_LENGTH: usize = 120;

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const RESET: &str = "\x1b[0m";

/// Something in Claude's stream worth showing, or remembering.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The run has started, in this conversation, on this model.
    Started {
        conversation: String,
        model: Option<String>,
    },
    /// Claude said something.
    Said(String),
    /// Claude used a tool: its name, and the gist of what it asked of it.
    UsedTool { name: String, gist: String },
    /// A tool answered: the first line of its answer.
    ToolAnswered { first_line: String, failed: bool },
    /// The model was given this many tokens for its last message, the
    /// whole of the conversation so far: `model` is the model, as the
    /// message names it.
    Context { tokens: u64, model: Option<String> },
    /// The user asked this, in the transcript Claude Code keeps: a stream
    /// has the prompts crystal sent it, which crystal draws itself.
    Asked(String),
    /// The run is over.
    Finished(Outcome),
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub failed: bool,
    /// Claude's answer, or what went wrong.
    pub result: String,
    pub conversation: String,
    /// What Claude has cost so far, as it counts: from its process's start,
    /// so a process that has run several turns counts them all. A task
    /// shows each turn's own.
    pub cost_usd: f64,
    pub duration_ms: u64,
    /// The tools Claude asked for that weren't allowed: nobody is there to
    /// say yes to a task. Each one's name and gist.
    pub refused: Vec<String>,
    /// How many tokens each model the run used takes, by its name: its
    /// context window.
    pub windows: Vec<(String, u64)>,
}

/// The events in one line of the stream. One assistant message can say
/// something and then use a tool, so a line can hold several. A line that
/// isn't JSON, or is about something not shown here, holds none.
pub fn events(line: &str) -> Vec<Event> {
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    events_of(&event)
}

/// The events in one line of the transcript Claude Code keeps of a
/// conversation, `<id>.jsonl`: what the user asked, then what [`events`]
/// finds in a stream's line. A subagent's messages, and what Claude Code
/// writes about the user's own commands, like `/clear`, are left out.
pub fn kept_events(line: &str) -> Vec<Event> {
    let Ok(event) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if event["isSidechain"] == true || event["isMeta"] == true {
        return Vec::new();
    }
    let mut events: Vec<Event> = asked(&event).into_iter().map(Event::Asked).collect();
    events.extend(events_of(&event));
    events
}

/// What the user asked in `event`, a line of the transcript Claude Code
/// keeps: a prompt's text, or its text pieces; none for a tool's answer, or
/// what Claude Code writes about the user's own commands.
fn asked(event: &Value) -> Vec<String> {
    if event["type"] != "user" {
        return Vec::new();
    }
    let said: Vec<&str> = match &event["message"]["content"] {
        Value::String(text) => vec![text.as_str()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect(),
        _ => Vec::new(),
    };
    said.into_iter()
        .filter(|text| !text.starts_with("<command-") && !text.starts_with("<local-command-"))
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .collect()
}

fn events_of(event: &Value) -> Vec<Event> {
    // A subagent's messages carry the tool use that started it. Only the
    // main conversation is shown, to keep the transcript short.
    if !event["parent_tool_use_id"].is_null() {
        return Vec::new();
    }
    match event["type"].as_str() {
        Some("system") if event["subtype"] == "init" => started(event).into_iter().collect(),
        Some("assistant") => {
            let mut events: Vec<Event> = content(event).iter().filter_map(said_or_used).collect();
            events.extend(context(&event["message"]));
            events
        }
        Some("user") => content(event).iter().filter_map(answered).collect(),
        Some("result") => vec![Event::Finished(outcome(event))],
        _ => Vec::new(),
    }
}

/// The lines that start a run with `prompt`.
pub fn prompt_lines(prompt: &str) -> String {
    let mut lines = String::new();
    for line in prompt.lines() {
        let line = printable::line(line);
        lines.push_str(&format!("{BOLD}> {line}{RESET}\r\n"));
    }
    lines.push_str("\r\n");
    lines
}

/// The lines that show `event`, on a screen `width` columns wide.
pub fn lines(event: &Event, width: u16) -> String {
    match event {
        Event::Started { .. } | Event::Context { .. } => String::new(),
        Event::Asked(prompt) => prompt_lines(prompt),
        Event::Said(text) => said_lines(text, width),
        Event::UsedTool { name, gist } => {
            let (name, gist) = (printable::line(name), printable::line(gist));
            format!("{CYAN}▸ {name}{RESET} {gist}\r\n")
        }
        Event::ToolAnswered { first_line, failed } => {
            let color = if *failed { RED } else { DIM };
            let first_line = printable::line(first_line);
            format!("{color}  └ {first_line}{RESET}\r\n")
        }
        Event::Finished(outcome) => finished_lines(outcome),
    }
}

/// What Claude said, laid out as a markdown page `width` columns wide, in
/// the terminal's own colors.
fn said_lines(text: &str, width: u16) -> String {
    let mut lines = String::new();
    let text = printable::text(text);
    for line in markdown::render(&text, usize::from(width).max(1)) {
        for piece in line {
            // Markdown reads a character reference like `&#x1b;` as the
            // character, so a piece can hold what the text didn't.
            let text = printable::line(&piece.text);
            let codes = sgr(piece.mark);
            if codes.is_empty() {
                lines.push_str(&text);
            } else {
                lines.push_str(&format!("\x1b[{codes}m{text}{RESET}"));
            }
        }
        lines.push_str("\r\n");
    }
    lines
}

/// The SGR parameters a piece of a page marked `mark` is drawn with: the
/// terminal's sixteen colors, and no surface behind code, since none could
/// be picked without knowing them.
fn sgr(mark: Mark) -> String {
    let color = match mark.ink {
        Ink::Text | Ink::Token(TokenKind::Text) => None,
        Ink::Muted | Ink::Rule | Ink::Token(TokenKind::Comment) => Some("2"),
        Ink::Accent => Some("35"),
        Ink::Code => Some("36"),
        Ink::Token(TokenKind::Keyword) => Some("34"),
        Ink::Done | Ink::Token(TokenKind::String) => Some("32"),
        Ink::Warning | Ink::Token(TokenKind::Number) => Some("33"),
        Ink::Failed => Some("31"),
    };
    let modifiers = [
        (Modifier::BOLD, "1"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::CROSSED_OUT, "9"),
    ];
    let mut codes: Vec<&str> = modifiers
        .into_iter()
        .filter(|(modifier, _)| mark.modifier.contains(*modifier))
        .map(|(_, code)| code)
        .collect();
    codes.extend(color);
    codes.join(";")
}

/// The lines for a note between runs, dimmed so it isn't taken for
/// Claude's.
pub fn note_lines(note: &str) -> String {
    let note = printable::line(note);
    format!("{DIM}{note}{RESET}\r\n\r\n")
}

/// The line for a permission Claude asks for, which waits on the user.
pub fn asking_lines(tool: &str, gist: &str) -> String {
    let (tool, gist) = (printable::line(tool), printable::line(gist));
    format!("{YELLOW}⚠ {tool}{RESET} {gist} {DIM}· waiting on you{RESET}\r\n")
}

/// How the user answered a permission: `allowed`, `allowed always · <the
/// rule kept>`, `denied: <what Claude was told>`, or that Claude took it
/// back.
pub fn answered_lines(answer: &str) -> String {
    let color = if answer.starts_with("allowed") {
        GREEN
    } else {
        DIM
    };
    let answer = printable::line(answer);
    format!("{color}  └ {answer}{RESET}\r\n")
}

/// The lines for a run that ended without saying how: it crashed, or
/// was stopped. `why` is how the process ended, and `errors` the last of
/// what it wrote to its standard error.
pub fn cut_short_lines(why: &str, errors: &[String]) -> String {
    let why = printable::line(why);
    let mut lines = format!("\r\n{RED}✗ failed{RESET} · {why}\r\n");
    for error in errors {
        let error = printable::line(error);
        lines.push_str(&format!("{DIM}  {error}{RESET}\r\n"));
    }
    lines.push_str("\r\n");
    lines
}

fn finished_lines(outcome: &Outcome) -> String {
    let mut lines = String::from("\r\n");
    let took = duration(outcome.duration_ms);
    let cost = format!("${:.2}", outcome.cost_usd);
    if outcome.failed {
        let why = gist(&outcome.result);
        let why = printable::line(&why);
        lines.push_str(&format!(
            "{RED}✗ failed{RESET} · {why} · {took} · {cost}\r\n"
        ));
    } else {
        lines.push_str(&format!("{GREEN}✓ done{RESET} · {took} · {cost}\r\n"));
    }
    for refused in &outcome.refused {
        let refused = printable::line(refused);
        lines.push_str(&format!("{YELLOW}  refused: {refused}{RESET}\r\n"));
    }
    lines.push_str("\r\n");
    lines
}

fn started(event: &Value) -> Option<Event> {
    let conversation = event["session_id"].as_str()?.to_string();
    let model = event["model"].as_str().map(String::from);
    Some(Event::Started {
        conversation,
        model,
    })
}

/// A message's content blocks: text, tool uses, tool results.
fn content(event: &Value) -> Vec<Value> {
    match &event["message"]["content"] {
        Value::Array(blocks) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn said_or_used(block: &Value) -> Option<Event> {
    match block["type"].as_str()? {
        "text" => {
            let text = block["text"].as_str()?;
            (!text.trim().is_empty()).then(|| Event::Said(text.to_string()))
        }
        "tool_use" => {
            let name = block["name"].as_str()?.to_string();
            let gist = tool_gist(&name, &block["input"]);
            Some(Event::UsedTool { name, gist })
        }
        _ => None,
    }
}

/// How many tokens the model was given for `message`: what it read, from
/// its cache or not, and what it wrote, which the next message reads.
fn context(message: &Value) -> Option<Event> {
    let usage = &message["usage"];
    let count = |key: &str| usage[key].as_u64().unwrap_or(0);
    let tokens = count("input_tokens")
        + count("cache_creation_input_tokens")
        + count("cache_read_input_tokens")
        + count("output_tokens");
    (tokens > 0).then(|| Event::Context {
        tokens,
        model: message["model"].as_str().map(String::from),
    })
}

fn answered(block: &Value) -> Option<Event> {
    if block["type"] != "tool_result" {
        return None;
    }
    let text = match &block["content"] {
        Value::String(text) => text.clone(),
        // Or a list of pieces, of which the text ones count.
        Value::Array(pieces) => pieces
            .iter()
            .filter_map(|piece| piece["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let first_line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .map_or_else(|| "(nothing)".to_string(), gist);
    let failed = block["is_error"].as_bool().unwrap_or(false);
    Some(Event::ToolAnswered { first_line, failed })
}

fn outcome(event: &Value) -> Outcome {
    let failed = event["is_error"].as_bool().unwrap_or(false);
    Outcome {
        failed,
        result: result_text(event, failed),
        conversation: event["session_id"].as_str().unwrap_or_default().to_string(),
        cost_usd: event["total_cost_usd"].as_f64().unwrap_or(0.0),
        duration_ms: event["duration_ms"].as_u64().unwrap_or(0),
        refused: refused(event),
        windows: windows(event),
    }
}

/// How many tokens each model a run used takes, as its result says.
fn windows(event: &Value) -> Vec<(String, u64)> {
    let Value::Object(models) = &event["modelUsage"] else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|(model, usage)| Some((model.clone(), usage["contextWindow"].as_u64()?)))
        .collect()
}

/// Claude's answer. A failed run may have none, only the errors it ran
/// into, or only the kind of failure it was, like `error_max_turns`.
fn result_text(event: &Value, failed: bool) -> String {
    if let Some(result) = event["result"].as_str().filter(|text| !text.is_empty()) {
        return result.to_string();
    }
    let errors: Vec<&str> = match &event["errors"] {
        Value::Array(errors) => errors.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    if !errors.is_empty() {
        return errors.join("; ");
    }
    if failed {
        let kind = event["subtype"].as_str().unwrap_or("error");
        return kind.replace('_', " ");
    }
    String::new()
}

fn refused(event: &Value) -> Vec<String> {
    let Value::Array(denials) = &event["permission_denials"] else {
        return Vec::new();
    };
    denials
        .iter()
        .filter_map(|denial| {
            let name = denial["tool_name"].as_str()?;
            Some(format!("{name} {}", tool_gist(name, &denial["tool_input"])))
        })
        .collect()
}

/// What a tool was asked to do, in a few words: the command a shell ran,
/// the file read or written, the pattern searched for. Anything else shows
/// its input as it is.
pub fn tool_gist(name: &str, input: &Value) -> String {
    let key = match name {
        "Bash" => "command",
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => "file_path",
        "Grep" | "Glob" => "pattern",
        "WebFetch" => "url",
        "WebSearch" => "query",
        "Task" | "Agent" => "description",
        _ => "",
    };
    match input[key].as_str() {
        Some(text) => gist(text),
        None => gist(&input.to_string()),
    }
}

/// `text` on one line, cut down to [`GIST_LENGTH`] characters.
fn gist(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() <= GIST_LENGTH {
        return first_line.to_string();
    }
    let cut: String = first_line.chars().take(GIST_LENGTH).collect();
    format!("{cut}…")
}

/// `ms` as people say it: "4.6s", "1m 12s".
fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    if seconds < 60 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_event_names_the_conversation() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc","cwd":"/x","model":"claude-opus-5-5"}"#;
        assert_eq!(
            events(line),
            [Event::Started {
                conversation: "abc".into(),
                model: Some("claude-opus-5-5".into()),
            }]
        );
    }

    #[test]
    fn a_message_can_say_something_and_use_a_tool() {
        let line = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[
            {"type":"text","text":"Running the tests."},
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test","description":"Test"}}
        ]}}"#;
        assert_eq!(
            events(line),
            [
                Event::Said("Running the tests.".into()),
                Event::UsedTool {
                    name: "Bash".into(),
                    gist: "cargo test".into()
                },
            ]
        );
    }

    #[test]
    fn a_tool_answer_shows_its_first_line_whatever_shape_it_comes_in() {
        let text = r#"{"type":"user","parent_tool_use_id":null,"message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":"\nok. 3 passed\nmore","is_error":false}]}}"#;
        assert_eq!(
            events(text),
            [Event::ToolAnswered {
                first_line: "ok. 3 passed".into(),
                failed: false
            }]
        );
        let pieces = r#"{"type":"user","parent_tool_use_id":null,"message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":[{"type":"text","text":"no such file"}],"is_error":true}]}}"#;
        assert_eq!(
            events(pieces),
            [Event::ToolAnswered {
                first_line: "no such file".into(),
                failed: true
            }]
        );
    }

    /// The outcome a result line holds.
    fn outcome_of(line: &str) -> Outcome {
        match events(line).pop() {
            Some(Event::Finished(outcome)) => outcome,
            other => panic!("no outcome in {line}, only {other:?}"),
        }
    }

    #[test]
    fn the_result_says_how_the_run_ended_and_what_was_refused() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"All green.",
            "session_id":"abc","total_cost_usd":0.042,"duration_ms":4600,
            "permission_denials":[{"tool_name":"Bash","tool_use_id":"t9","tool_input":{"command":"rm -rf build"}}]}"#;
        let outcome = outcome_of(line);
        assert!(!outcome.failed);
        assert_eq!(outcome.result, "All green.");
        assert_eq!(outcome.conversation, "abc");
        assert_eq!(outcome.cost_usd, 0.042);
        assert_eq!(outcome.refused, ["Bash rm -rf build"]);
    }

    #[test]
    fn a_failed_run_says_what_went_wrong_even_without_a_result() {
        let line =
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"session_id":"abc"}"#;
        let outcome = outcome_of(line);
        assert!(outcome.failed);
        assert_eq!(outcome.result, "error max turns");
    }

    #[test]
    fn a_message_says_how_full_the_context_is_and_a_result_how_much_each_model_takes() {
        let said = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"model":"claude-haiku-4-5",
            "content":[{"type":"text","text":"ok"}],
            "usage":{"input_tokens":10,"cache_creation_input_tokens":10479,"cache_read_input_tokens":200,"output_tokens":3}}}"#;
        assert_eq!(
            events(said),
            [
                Event::Said("ok".into()),
                Event::Context {
                    tokens: 10_692,
                    model: Some("claude-haiku-4-5".into())
                }
            ]
        );
        let result = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"abc",
            "modelUsage":{"claude-haiku-4-5":{"inputTokens":10,"contextWindow":200000},"claude-x":{"inputTokens":1}}}"#;
        assert_eq!(
            outcome_of(result).windows,
            [("claude-haiku-4-5".to_string(), 200_000)]
        );
    }

    #[test]
    fn claude_code_s_transcript_has_the_prompts_too_but_not_its_own_notes() {
        let asked = r#"{"type":"user","message":{"role":"user","content":"fix the tests"},"isSidechain":false}"#;
        assert_eq!(kept_events(asked), [Event::Asked("fix the tests".into())]);
        let pieces =
            r#"{"type":"user","message":{"content":[{"type":"text","text":"and the docs"}]}}"#;
        assert_eq!(kept_events(pieces), [Event::Asked("and the docs".into())]);
        let command =
            r#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#;
        assert!(kept_events(command).is_empty());
        let meta = r#"{"type":"user","isMeta":true,"message":{"content":"Caveat: …"}}"#;
        assert!(kept_events(meta).is_empty());
        let subagent = r#"{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"inner"}]}}"#;
        assert!(kept_events(subagent).is_empty());
        let answered = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        assert_eq!(
            kept_events(answered),
            [Event::ToolAnswered {
                first_line: "ok".into(),
                failed: false
            }]
        );
        // Drawn as the prompts crystal sends are.
        assert_eq!(
            lines(&Event::Asked("fix it".into()), 80),
            prompt_lines("fix it")
        );
    }

    #[test]
    fn what_isnt_shown_gives_nothing() {
        assert!(events("not json").is_empty());
        assert!(events(r#"{"type":"rate_limit_event"}"#).is_empty());
        let subagent = r#"{"type":"assistant","parent_tool_use_id":"t1","message":{"content":[{"type":"text","text":"inner"}]}}"#;
        assert!(events(subagent).is_empty());
    }

    #[test]
    fn tools_show_what_they_were_asked_to_do() {
        let read = serde_json::json!({"file_path": "src/main.rs"});
        assert_eq!(tool_gist("Read", &read), "src/main.rs");
        let other = serde_json::json!({"server": "x"});
        assert_eq!(tool_gist("mcp__x__y", &other), r#"{"server":"x"}"#);
        let long = "a".repeat(200);
        assert_eq!(gist(&long).chars().count(), GIST_LENGTH + 1);
    }

    #[test]
    fn lines_are_terminal_lines() {
        let used = lines(
            &Event::UsedTool {
                name: "Bash".into(),
                gist: "cargo test".into(),
            },
            80,
        );
        assert!(used.contains("▸ Bash") && used.contains("cargo test"));
        assert!(used.ends_with("\r\n"));
        assert_eq!(
            prompt_lines("fix it\nplease"),
            format!("{BOLD}> fix it{RESET}\r\n{BOLD}> please{RESET}\r\n\r\n")
        );
    }

    #[test]
    fn the_end_of_a_run_shows_how_long_it_took_and_its_cost() {
        let outcome = Outcome {
            failed: false,
            result: "ok".into(),
            conversation: "abc".into(),
            cost_usd: 0.0421,
            duration_ms: 72_000,
            refused: vec!["Bash rm -rf build".into()],
            windows: Vec::new(),
        };
        let shown = lines(&Event::Finished(outcome), 80);
        assert!(shown.contains("✓ done"));
        assert!(shown.contains("1m 12s"));
        assert!(shown.contains("$0.04"));
        assert!(shown.contains("refused: Bash rm -rf build"));
    }

    #[test]
    fn a_permission_shows_what_it_asks_then_its_answer() {
        let asked = asking_lines("Bash", "cargo test");
        assert!(asked.contains("⚠ Bash") && asked.contains("cargo test"));
        assert!(asked.contains("waiting on you"));
        assert!(answered_lines("allowed always · Bash(cargo test:*)").starts_with(GREEN));
        assert!(answered_lines("denied").starts_with(DIM));
    }

    #[test]
    fn what_claude_says_is_a_markdown_page_as_wide_as_the_screen() {
        let said = Event::Said("# Done\n\nThe **tests** pass: `cargo test`.\n".into());
        assert_eq!(
            lines(&said, 80),
            format!(
                "\x1b[1;35mDone{RESET}\r\n\x1b[2m━━━━{RESET}\r\n\r\n\
                 The \x1b[1mtests{RESET} pass: \x1b[36mcargo test{RESET}.\r\n"
            )
        );
        let long = Event::Said("one two three four five".into());
        assert_eq!(lines(&long, 10), "one two\r\nthree four\r\nfive\r\n");
    }

    /// Every kind of line the screen is drawn with, each made with `text`
    /// wherever crystal puts something it didn't write.
    fn every_line(text: &str) -> String {
        let outcome = |failed| Outcome {
            failed,
            result: text.into(),
            conversation: "abc".into(),
            cost_usd: 0.0,
            duration_ms: 0,
            refused: vec![text.into()],
            windows: Vec::new(),
        };
        let used = Event::UsedTool {
            name: text.into(),
            gist: text.into(),
        };
        let answered = Event::ToolAnswered {
            first_line: text.into(),
            failed: true,
        };
        [
            prompt_lines(text),
            lines(&Event::Said(text.into()), 80),
            lines(&used, 80),
            lines(&answered, 80),
            // A prompt in the transcript Claude Code keeps, drawn again.
            lines(&Event::Asked(text.into()), 80),
            lines(&Event::Finished(outcome(true)), 80),
            lines(&Event::Finished(outcome(false)), 80),
            note_lines(text),
            asking_lines(text, text),
            answered_lines(text),
            cut_short_lines(text, &[text.to_string()]),
        ]
        .concat()
    }

    /// Holds `drawn` to crystal's own colors and line ends.
    fn assert_only_colors_and_text(drawn: &str, from: &str) {
        for order in printable::orders(drawn) {
            let color = order.starts_with("\x1b[") && order.ends_with('m');
            assert!(
                color || order == "\r" || order == "\n",
                "{order:?} from {from:?}"
            );
        }
        assert!(!drawn.replace("\r\n", "").contains('\r'), "{from:?}");
    }

    #[test]
    fn nothing_claude_or_a_tool_says_is_taken_as_an_order() {
        for hostile in printable::HOSTILE {
            let drawn = every_line(hostile);
            assert_only_colors_and_text(&drawn, hostile);
            let mut screen = crate::vt::Screen::new(80, 100);
            screen.process(drawn.as_bytes());
            assert_eq!(screen.title(), "", "{hostile:?}");
            assert_eq!(screen.take_copied(), None, "{hostile:?}");
            assert!(!screen.alternate_screen(), "{hostile:?}");
        }
    }

    #[test]
    fn a_character_reference_in_what_claude_says_stays_text() {
        let said = "&#x1b;]0;pwned&#7; then &#27;[?1049h and &#x9b;2J";
        let drawn = lines(&Event::Said(said.into()), 80);
        assert_only_colors_and_text(&drawn, said);
        assert!(drawn.contains("]0;pwned then [?1049h and 2J"), "{drawn:?}");
    }

    #[test]
    fn durations_read_the_way_people_say_them() {
        assert_eq!(duration(4600), "4.6s");
        assert_eq!(duration(125_000), "2m 5s");
    }
}
