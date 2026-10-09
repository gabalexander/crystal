//! Running Claude Code for the wiki: `claude -p` in the checkout of the
//! commit, able to read it and nothing else.
//!
//! Each run is `--restricted`, which takes away every tool that runs a
//! command or fetches from the web, confines the file tools to the
//! checkout, and reads none of the user's, the project's or anyone's
//! settings files, so neither their permissions nor their hooks reach it.
//! It has `Read`, `Grep` and `Glob`, no MCP server, no session kept, a
//! turn cap, a budget, and `--json-schema` for the shape of its answer.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// The tools a writer has: reading the checkout, and nothing else.
pub const TOOLS: &str = "Read,Grep,Glob";

/// Every tool that could change something or reach past the checkout,
/// refused besides, for a Claude that would offer one anyway.
const REFUSED: &str = "Bash,Edit,Write,MultiEdit,NotebookEdit,WebFetch,WebSearch,Agent,Task";

/// Settings over none: no hook runs for it.
const SETTINGS: &str = r#"{"disableAllHooks":true}"#;

/// One run of Claude: what it's told, how much it may do.
#[derive(Debug, Clone)]
pub struct Ask {
    pub model: String,
    /// Appended to Claude Code's own system prompt, which says how to use
    /// its tools.
    pub system: String,
    pub schema: Value,
    /// The message, on its standard input.
    pub message: String,
    pub max_turns: u32,
    pub budget_usd: f64,
    pub timeout: Duration,
    /// Whether it may read the checkout; without, it has no tools.
    pub tools: bool,
}

/// What a run answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// Its structured output.
    pub value: Value,
    pub cost_usd: f64,
    /// The model that answered, as Claude Code names it:
    /// `claude-sonnet-5-5`.
    pub model: Option<String>,
}

/// A run that failed, with what it cost.
#[derive(Debug)]
pub struct Failed {
    pub why: String,
    pub cost_usd: f64,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

impl std::error::Error for Failed {}

/// Claude's arguments for `ask`.
pub fn args(ask: &Ask) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--restricted",
        "--output-format",
        "json",
        "--model",
        &ask.model,
        "--append-system-prompt",
        &ask.system,
        "--json-schema",
        &ask.schema.to_string(),
        "--tools",
        if ask.tools { TOOLS } else { "" },
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect();
    if ask.tools {
        args.extend(["--allowedTools".to_string(), TOOLS.to_string()]);
    }
    args.extend(
        [
            "--disallowedTools",
            REFUSED,
            "--strict-mcp-config",
            "--settings",
            SETTINGS,
            "--no-session-persistence",
            "--max-turns",
        ]
        .iter()
        .map(|arg| arg.to_string()),
    );
    args.push(ask.max_turns.to_string());
    args.push("--max-budget-usd".to_string());
    args.push(format!("{:.2}", ask.budget_usd.max(0.01)));
    args
}

/// Runs Claude on `ask` in `cwd`, and gives back its answer; or a
/// [`Failed`], with what it cost, as the error.
pub fn run(ask: &Ask, cwd: &Path) -> Result<Answer> {
    let mut child = Command::new("claude")
        .args(args(ask))
        .current_dir(cwd)
        // It isn't a session of crystal's, whose hooks would tell of it.
        .env_remove("CRYSTAL_SESSION")
        .env_remove("CRYSTAL_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("couldn't start claude: is Claude Code installed and on the PATH?")?;
    let mut stdin = child.stdin.take().expect("its input is piped");
    let message = ask.message.clone();
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
    let deadline = Instant::now() + ask.timeout;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failed {
                why: format!(
                    "it took longer than {}s, and was stopped",
                    ask.timeout.as_secs()
                ),
                cost_usd: 0.0,
            }
            .into());
        }
        thread::sleep(Duration::from_millis(200));
    }
    let out = stdout.join().unwrap_or_default();
    let errors = stderr.join().unwrap_or_default();
    answer_of(&out).map_err(|failed| {
        let why = match errors.lines().rev().find(|line| !line.trim().is_empty()) {
            Some(said) if failed.why.starts_with("there's no result") => {
                let said = said.trim();
                if said.contains("--restricted") {
                    format!(
                        "claude said: {said} (the wiki needs a Claude Code that knows --restricted)"
                    )
                } else {
                    format!("claude said: {said}")
                }
            }
            _ => failed.why,
        };
        Failed {
            why,
            cost_usd: failed.cost_usd,
        }
        .into()
    })
}

/// The answer in what `claude -p --output-format json` wrote.
fn answer_of(out: &str) -> std::result::Result<Answer, Failed> {
    let result = out
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|value| value["type"] == "result")
        .ok_or_else(|| Failed {
            why: "there's no result in what it wrote".into(),
            cost_usd: 0.0,
        })?;
    let cost_usd = result["total_cost_usd"].as_f64().unwrap_or(0.0);
    if result["is_error"] == true || result["subtype"] != "success" {
        let subtype = result["subtype"].as_str().unwrap_or("error");
        let said = result["result"].as_str().unwrap_or_default();
        let why = match subtype {
            "error_max_budget_usd" => "it reached its budget before it answered".to_string(),
            "error_max_turns" => "it ran out of turns before it answered".to_string(),
            _ => format!("its run ended with {subtype}: {}", said.trim()),
        };
        return Err(Failed { why, cost_usd });
    }
    let value = match &result["structured_output"] {
        Value::Null => result["result"]
            .as_str()
            .and_then(|text| serde_json::from_str(text.trim()).ok())
            .ok_or_else(|| Failed {
                why: "its answer had no structured output".into(),
                cost_usd,
            })?,
        value => value.clone(),
    };
    // The model that did most of the work: the one that cost most.
    let model = result["modelUsage"].as_object().and_then(|usage| {
        usage
            .iter()
            .max_by(|a, b| {
                let cost = |v: &Value| v["costUSD"].as_f64().unwrap_or(0.0);
                cost(a.1).total_cmp(&cost(b.1))
            })
            .map(|(name, _)| name.clone())
    });
    Ok(Answer {
        value,
        cost_usd,
        model,
    })
}

/// What a failed run cost, from the error [`run`] gave.
pub fn cost_of(err: &anyhow::Error) -> f64 {
    err.downcast_ref::<Failed>()
        .map_or(0.0, |failed| failed.cost_usd)
}

/// A model's name as people say it: `claude-sonnet-5-5` is `Claude Sonnet
/// 5.5`, `claude-haiku-4-5-20251001` `Claude Haiku 4.5`; anything else as
/// it is.
pub fn display_name(model: &str) -> String {
    let Some(rest) = model.strip_prefix("claude-") else {
        return model.to_string();
    };
    let mut words = Vec::new();
    let mut version = Vec::new();
    for part in rest.split('-') {
        if part.chars().all(|c| c.is_ascii_digit()) {
            // A date at its end isn't its version.
            if part.len() < 8 {
                version.push(part);
            }
        } else {
            let mut chars = part.chars();
            let word: String = chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
            words.push(word);
        }
    }
    let mut name = format!("Claude {}", words.join(" "));
    if !version.is_empty() {
        name.push(' ');
        name.push_str(&version.join("."));
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask() -> Ask {
        Ask {
            model: "sonnet".into(),
            system: "You write.".into(),
            schema: json!({"type": "object"}),
            message: "go".into(),
            max_turns: 12,
            budget_usd: 1.5,
            timeout: Duration::from_secs(60),
            tools: true,
        }
    }

    #[test]
    fn it_reads_the_checkout_and_nothing_else() {
        let args = args(&ask());
        let after = |flag: &str| {
            let at = args.iter().position(|arg| arg == flag).unwrap();
            args[at + 1].clone()
        };
        assert_eq!(args[..2], ["-p", "--restricted"]);
        assert_eq!(after("--tools"), "Read,Grep,Glob");
        assert_eq!(after("--allowedTools"), "Read,Grep,Glob");
        assert!(after("--disallowedTools").contains("Bash"));
        assert!(after("--disallowedTools").contains("WebFetch"));
        assert!(args.contains(&"--strict-mcp-config".to_string()));
        assert!(args.contains(&"--no-session-persistence".to_string()));
        assert_eq!(after("--settings"), r#"{"disableAllHooks":true}"#);
        assert_eq!(after("--max-turns"), "12");
        assert_eq!(after("--max-budget-usd"), "1.50");
        let none = args_without_tools();
        assert!(
            none.windows(2)
                .any(|w| w[0] == "--tools" && w[1].is_empty())
        );
        assert!(!none.contains(&"--allowedTools".to_string()));
    }

    fn args_without_tools() -> Vec<String> {
        args(&Ask {
            tools: false,
            ..ask()
        })
    }

    #[test]
    fn the_answer_is_its_structured_output_its_cost_and_its_model() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{"a":1},"total_cost_usd":0.25,"modelUsage":{"claude-haiku-5-5":{"costUSD":0.01},"claude-sonnet-5-5":{"costUSD":0.24}}}"#;
        let answer = answer_of(out).unwrap();
        assert_eq!(answer.value, json!({"a": 1}));
        assert_eq!(answer.cost_usd, 0.25);
        assert_eq!(answer.model.as_deref(), Some("claude-sonnet-5-5"));
        let budget = r#"{"type":"result","subtype":"error_max_budget_usd","is_error":true,"total_cost_usd":1.0}"#;
        let failed = answer_of(budget).unwrap_err();
        assert_eq!(failed.cost_usd, 1.0);
        assert!(failed.why.contains("budget"));
        assert!(answer_of("nothing").is_err());
    }

    #[test]
    fn a_model_is_named_as_people_say_it() {
        assert_eq!(display_name("claude-sonnet-5-5"), "Claude Sonnet 5.5");
        assert_eq!(
            display_name("claude-haiku-4-5-20251001"),
            "Claude Haiku 4.5"
        );
        assert_eq!(display_name("claude-opus-4"), "Claude Opus 4");
        assert_eq!(display_name("sonnet"), "sonnet");
    }
}
