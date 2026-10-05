//! `crystal mcp`: an MCP server over its standard input and output, which
//! crystal gives every Claude Code session it starts, in a terminal or a
//! task in the background, to search its project's memory with
//! `memory_search` and read an entry in full with `memory_show`. With tools
//! of its own, it reads what earlier sessions learned, beyond the few
//! entries it's shown as it starts, without a shell command, which a task
//! has nobody to say yes to.
//!
//! It speaks JSON-RPC 2.0, a message a line, as MCP's stdio transport does,
//! and only as much of MCP as a server of two tools needs: `initialize`,
//! `ping`, `tools/list` and `tools/call`. It reads the memory's database
//! itself, so it needs no daemon.

use crate::memory::{self, Kind, Listed, Store, Wanted};
use crate::memory_cli;
use crate::output;
use crate::tui::sidebar::ago;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The server's name, which its tools are known to Claude by:
/// `mcp__crystal__memory_search`.
pub const SERVER: &str = "crystal";

/// The tools, as Claude's `--allowedTools` names them.
pub const TOOLS: [&str; 2] = ["mcp__crystal__memory_search", "mcp__crystal__memory_show"];

/// The version of MCP it speaks when the client doesn't say which.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// How many entries a search gives when it isn't told.
const DEFAULT_LIMIT: u64 = 10;

/// Serves the memory of the project `dir` is in, kept for the daemon at
/// `socket`, until its input ends.
pub fn run(socket: &Path, dir: &Path) -> Result<()> {
    let server = Server {
        socket: socket.to_path_buf(),
        project: memory::project_of(dir),
        on: memory::enabled_now,
        search: crate::memory_cli::found,
    };
    let mut out = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.answer(&line) {
            // With Claude Code gone, the server ends quietly.
            writeln!(out, "{reply}")
                .and_then(|()| out.flush())
                .map_err(output::failed)?;
        }
    }
    Ok(())
}

/// The command line that runs the server for a session in `dir`, as
/// Claude's `--mcp-config` takes it.
pub fn config(crystal: &Path, socket: &Path, dir: &Path) -> String {
    let path = |path: &Path| path.to_string_lossy().into_owned();
    json!({
        "mcpServers": {
            SERVER: {
                "type": "stdio",
                "command": path(crystal),
                "args": ["--socket", path(socket), "mcp", "-C", path(dir)],
            }
        }
    })
    .to_string()
}

struct Server {
    socket: PathBuf,
    project: PathBuf,
    /// Whether memory is on, asked afresh at each call: with it off, the
    /// tools say so, and do nothing.
    on: fn() -> bool,
    /// How it searches: through the daemon, which keeps the model that
    /// searches by meaning loaded once for every task.
    search: Search,
}

/// A search of a project's memory: the socket, the project, the words,
/// and what it keeps to and the most to give.
type Search = fn(&Path, &Path, &str, &Wanted) -> Result<Vec<Listed>>;

impl Server {
    /// The reply to one message, or `None` for a notification, which
    /// isn't answered.
    fn answer(&self, line: &str) -> Option<Value> {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return Some(error(Value::Null, -32700, "that isn't JSON"));
        };
        let id = message.get("id").cloned()?;
        let params = &message["params"];
        let result = match message["method"].as_str().unwrap_or_default() {
            "initialize" => initialize(params),
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tools() }),
            "tools/call" => self.call(params),
            method => return Some(error(id, -32601, &format!("there's no method {method}"))),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    /// What a tool answers, as MCP gives it: text, marked as an error when
    /// it is one, for the model to read either way.
    fn call(&self, params: &Value) -> Value {
        let arguments = &params["arguments"];
        let answer = match params["name"].as_str().unwrap_or_default() {
            "memory_search" => self.search(arguments),
            "memory_show" => self.show(arguments),
            name => Err(anyhow::anyhow!("there's no tool {name}")),
        };
        match answer {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
            Err(err) => json!({
                "content": [{ "type": "text", "text": format!("{err:#}") }],
                "isError": true,
            }),
        }
    }

    fn check_on(&self) -> Result<()> {
        if !(self.on)() {
            bail!(crate::plugins::off("memory"));
        }
        Ok(())
    }

    fn search(&self, arguments: &Value) -> Result<String> {
        self.check_on()?;
        let query = arguments["query"].as_str().unwrap_or_default();
        let kind = match arguments["kind"].as_str() {
            Some(name) => Some(Kind::parse(name).with_context(|| format!("{name} isn't a kind"))?),
            None => None,
        };
        let limit = arguments["limit"].as_u64().unwrap_or(DEFAULT_LIMIT);
        let limit = limit.clamp(1, memory::SEARCH_LIMIT as u64) as usize;
        // Stale entries too, marked: the model can tell.
        let wanted = Wanted {
            kind,
            ..Wanted::best(limit)
        };
        let found = (self.search)(&self.socket, &self.project, query, &wanted)?;
        if found.is_empty() {
            return Ok("Nothing in this project's memory matches that.".to_string());
        }
        let now = now();
        let lines: Vec<String> = found.iter().map(|item| row(item, now)).collect();
        Ok(lines.join("\n"))
    }

    fn show(&self, arguments: &Value) -> Result<String> {
        self.check_on()?;
        let id = arguments["id"]
            .as_u64()
            .or_else(|| {
                arguments["id"]
                    .as_str()?
                    .trim_start_matches('#')
                    .parse()
                    .ok()
            })
            .context("say which entry, by its id")?;
        let entry = Store::open(&self.socket)?
            .get(&self.project, id)?
            .with_context(|| format!("there's no entry {id}"))?;
        let freshness = memory::freshness(&entry, &self.project);
        Ok(memory_cli::in_full(&entry, freshness, now()))
    }
}

fn initialize(params: &Value) -> Value {
    let version = params["protocolVersion"]
        .as_str()
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER, "version": env!("CARGO_PKG_VERSION") },
        "instructions": "This project's memory: what earlier sessions working here learned. \
                         Search it before working out something that may be known already.",
    })
}

fn tools() -> Value {
    let kinds: Vec<String> = Kind::ALL.iter().map(Kind::to_string).collect();
    json!([
        {
            "name": "memory_search",
            "description": "Search this project's memory: what earlier sessions working here \
                            learned, like decisions and why, gotchas, commands that work and \
                            notes. Give a few words or a question: any of the words matches, \
                            and so does a word they start or stem from; with search by meaning \
                            on, so does what means the same, and a search nothing answers finds \
                            nothing. Gives the best matches first, a line each: id, kind, age, \
                            text, the files it's about, [drifting] when some of those have \
                            changed since, and [stale] when all of them have.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to look for, in a few words." },
                    "kind": { "type": "string", "enum": kinds, "description": "Only entries of this kind." },
                    "limit": { "type": "integer", "minimum": 1, "maximum": memory::SEARCH_LIMIT, "description": "The most entries to give; 10 if not said." }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true }
        },
        {
            "name": "memory_show",
            "description": "Read one entry of this project's memory in full, by the id \
                            memory_search gives it: its text, files, where it came from, and \
                            how often and how lately it was said.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The entry's id." }
                },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": true }
        }
    ])
}

/// An entry as a search gives it, on one line.
fn row(item: &Listed, now: u64) -> String {
    let entry = &item.entry;
    let mut line = format!(
        "{} · {} · {} · {}",
        entry.id,
        entry.kind,
        ago(entry.last_seen, now),
        memory::title(&entry.text)
    );
    if !entry.files.is_empty() {
        line.push_str(&format!(" ({})", entry.files.join(", ")));
    }
    if let Some(mark) = item.freshness.mark() {
        line.push_str(&format!(" [{mark}]"));
    }
    line
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn now() -> u64 {
    memory::seconds_since_epoch(SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{New, Source};

    fn server() -> (tempfile::TempDir, Server) {
        let dir = tempfile::tempdir().unwrap();
        let server = Server {
            socket: dir.path().join("crystal.sock"),
            project: dir.path().join("app"),
            on: || true,
            search: |socket, project, query, wanted| {
                Store::open(socket)?.find(project, query, wanted, None)
            },
        };
        std::fs::create_dir(&server.project).unwrap();
        let mut store = Store::open(&server.socket).unwrap();
        for (kind, text) in [
            (Kind::Gotcha, "The ledger tests need redis up"),
            (Kind::Decision, "Fees are kept in cents"),
        ] {
            let new = New {
                kind,
                text: text.into(),
                files: Vec::new(),
                source: Source::Session("fixer".into()),
                checkout: None,
            };
            store.add(&server.project, new).unwrap();
        }
        (dir, server)
    }

    fn ask(server: &Server, message: Value) -> Value {
        server.answer(&message.to_string()).unwrap()
    }

    fn call(server: &Server, name: &str, arguments: Value) -> Value {
        let message = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                             "params": {"name": name, "arguments": arguments}});
        ask(server, message)["result"].clone()
    }

    fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn it_says_who_it_is_and_lists_its_two_tools() {
        let (_dir, server) = server();
        let reply = ask(
            &server,
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                   "params": {"protocolVersion": "2025-03-26", "capabilities": {}}}),
        );
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], "crystal");
        assert!(reply["result"]["capabilities"]["tools"].is_object());

        let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(server.answer(&initialized.to_string()), None);

        let tools = ask(
            &server,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        );
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["memory_search", "memory_show"]);
        let full: Vec<String> = names
            .iter()
            .map(|n| format!("mcp__{SERVER}__{n}"))
            .collect();
        assert_eq!(full, TOOLS);
    }

    #[test]
    fn memory_search_gives_the_matches_a_line_each() {
        let (_dir, server) = server();
        let result = call(&server, "memory_search", json!({"query": "ledger test"}));
        assert_eq!(result["isError"], Value::Null);
        assert!(
            text(&result).starts_with("1 · gotcha · "),
            "{}",
            text(&result)
        );
        assert!(text(&result).ends_with("The ledger tests need redis up"));

        let result = call(
            &server,
            "memory_search",
            json!({"query": "", "kind": "decision"}),
        );
        assert!(text(&result).contains("Fees are kept in cents"));
        assert!(!text(&result).contains("ledger"));

        let result = call(&server, "memory_search", json!({"query": "deploys"}));
        assert_eq!(
            text(&result),
            "Nothing in this project's memory matches that."
        );

        let result = call(
            &server,
            "memory_search",
            json!({"query": "x", "kind": "fact"}),
        );
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn memory_show_gives_an_entry_in_full() {
        let (_dir, server) = server();
        let result = call(&server, "memory_show", json!({"id": 2}));
        let shown = text(&result);
        assert!(
            shown.starts_with("2 · decision\n\nFees are kept in cents\n"),
            "{shown}"
        );
        assert!(shown.contains("from: session fixer"));
        assert!(shown.contains("said once"));

        let result = call(&server, "memory_show", json!({"id": 9}));
        assert_eq!(result["isError"], true);
        assert_eq!(text(&result), "there's no entry 9");
    }

    #[test]
    fn with_memory_off_its_tools_say_so() {
        let (_dir, mut server) = server();
        server.on = || false;
        let result = call(&server, "memory_search", json!({"query": "ledger"}));
        assert_eq!(result["isError"], true);
        assert!(text(&result).contains("the memory plugin is off"));
    }

    #[test]
    fn what_it_doesn_t_know_is_an_error_that_says_so() {
        let (_dir, server) = server();
        let reply = ask(
            &server,
            json!({"jsonrpc": "2.0", "id": 7, "method": "resources/list"}),
        );
        assert_eq!(reply["error"]["code"], -32601);
        assert_eq!(server.answer("{nope").unwrap()["error"]["code"], -32700);
        let result = call(&server, "memory_add", json!({}));
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn its_config_runs_crystal_on_the_task_s_project() {
        let config: Value = serde_json::from_str(&config(
            Path::new("/bin/crystal"),
            Path::new("/run/c.sock"),
            Path::new("/code/app"),
        ))
        .unwrap();
        let server = &config["mcpServers"]["crystal"];
        assert_eq!(server["command"], "/bin/crystal");
        assert_eq!(
            server["args"],
            json!(["--socket", "/run/c.sock", "mcp", "-C", "/code/app"])
        );
    }
}
