//! `crystal api schema`: the JSON Schema of what crystal says over its
//! socket, as herdr's `api schema` gives it, for a client of your own: the
//! requests a client sends, the responses the daemon answers with, the
//! events a subscription streams, the lines a TUI taking layout orders and
//! the daemon trade, and what `crystal api snapshot` prints.
//!
//! The schema is `docs/crystal-api.schema.json`, bundled in the binary as it
//! is. The tests make it again from the types themselves, which derive
//! `schemars::JsonSchema` in test builds alone, so a release never builds
//! schemars, and fail while the file isn't what they make: every type a
//! request, a response or an event reaches needs the derive, and
//! `CRYSTAL_UPDATE_API_SCHEMA=1 cargo test api_schema` writes the file
//! again once one changes.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fmt::Write;
use std::path::Path;

/// The schema, as `docs/crystal-api.schema.json` has it.
pub const JSON: &str = include_str!("../../docs/crystal-api.schema.json");

/// What `crystal api schema` prints: what the schema covers, a line each,
/// with how many kinds of request, response and event there are.
pub fn summary() -> Result<String> {
    let schema: Value = serde_json::from_str(JSON).context("the bundled API schema")?;
    let mut out = format!(
        "crystal's API schema, for crystal {}\n\n",
        env!("CARGO_PKG_VERSION")
    );
    let messages = schema["schemas"]
        .as_object()
        .context("the bundled API schema has no schemas")?;
    let width = messages.keys().map(String::len).max().unwrap_or(0);
    for (name, message) in messages {
        let title = message["title"].as_str().unwrap_or_default();
        write!(out, "{name:width$}  {title}").unwrap();
        let kinds = match name.as_str() {
            "request" => kinds(&schema, "Request").map(|count| (count, "type")),
            "response" => kinds(&schema, "Response").map(|count| (count, "type")),
            "event" => kinds(&schema, "EventKind").map(|count| (count, "event")),
            _ => None,
        };
        if let Some((count, by)) = kinds {
            write!(out, ": {count} kinds, by `{by}`").unwrap();
        }
        out.push('\n');
    }
    out.push_str(
        "\n`crystal api schema --json` prints the whole schema, and `--output PATH` writes it to \
         a file.\n",
    );
    Ok(out)
}

/// How many kinds the type `name` in the schema's `$defs` is one of.
fn kinds(schema: &Value, name: &str) -> Option<usize> {
    schema["$defs"][name]["oneOf"].as_array().map(Vec::len)
}

/// Writes the schema to `path`, as `--json` prints it.
pub fn write(path: &Path) -> Result<()> {
    std::fs::write(path, JSON).with_context(|| format!("couldn't write {}", path.display()))
}

/// The schema, made from the types: what `docs/crystal-api.schema.json`
/// must be. Every type is described as crystal reads it, so a field it can
/// do without is optional, though crystal may always write it.
#[cfg(test)]
fn document() -> Value {
    use crate::api::Snapshot;
    use crate::events::Event;
    use crate::layout::{Relayed, Report};
    use crate::protocol::{Request, Response};
    use serde_json::json;

    let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
    let message = |title: &str, description: &str, schema: schemars::Schema| {
        let mut message = json!({ "title": title, "description": description });
        message
            .as_object_mut()
            .unwrap()
            .extend(schema.to_value().as_object().unwrap().clone());
        message
    };
    let mut request = message(
        "what a client sends the daemon",
        "A request: one JSON object on a line, its kind in `type`, beside the version of the \
         crystal that sent it. The daemon answers it with one response, on a line of its own, \
         and hangs up, but for a `subscribe`, an `attach` and a `take_layout_orders`, which go \
         on after it.",
        generator.subschema_for::<Request>(),
    );
    request["properties"] = json!({
        "version": {
            "description": "The version of the crystal that sent it, as `crystal --version` \
                prints it. The daemon refuses a request from a crystal of another version than \
                its own, or one that doesn't say, but for a `shutdown` and a `handover`, which \
                every daemon takes whatever the version.",
            "type": "string",
        },
    });
    let response = message(
        "what the daemon answers",
        "A response: one JSON object on a line, its kind in `type`. An `error` says why the \
         request couldn't be carried out, and a `timed_out` that a wait gave up.",
        generator.subschema_for::<Response>(),
    );
    let event = message(
        "what a subscription streams",
        "An event: one JSON object on a line, its kind in `event`, as a `subscribe` streams them \
         after its `subscribed`, `crystal events --json` prints them and plugins' hooks are given \
         them. Only the fields its kind carries are there.",
        generator.subschema_for::<Event>(),
    );
    let layout_order = message(
        "what the daemon hands a TUI taking layout orders",
        "A layout order: after a `take_layout_orders` is answered `done`, the daemon writes the \
         TUI each command to carry out on a line, numbered for its answer.",
        generator.subschema_for::<Relayed>(),
    );
    let layout_report = message(
        "what that TUI tells the daemon",
        "A layout report: what a TUI taking layout orders writes back, a line at a time: that \
         it was used, that its terminal gained or lost the focus, and each order's answer.",
        generator.subschema_for::<Report>(),
    );
    let snapshot = message(
        "what `crystal api snapshot` prints",
        "A snapshot: everything a client that keeps its own picture of crystal starts from, \
         with the latest event's `seq` to follow on from.",
        generator.subschema_for::<Snapshot>(),
    );
    let mut defs: Vec<(String, Value)> = generator.take_definitions(true).into_iter().collect();
    defs.sort_by(|(a, _), (b, _)| a.cmp(b));
    let mut document = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "crystal API",
        "description": "What crystal's CLI and its daemon say to each other over the daemon's \
            socket: one JSON object a line, a request, then the response to it. An `attach` \
            goes on in bytes, not JSON: the session's output from the daemon, and from the \
            client frames of keys and sizes. `schemas` names each message, and `$defs` holds \
            the types they're made of.",
        "schema_version": 1,
        "schemas": {
            "request": request,
            "response": response,
            "event": event,
            "layout_order": layout_order,
            "layout_report": layout_report,
            "snapshot": snapshot,
        },
        "$defs": serde_json::Map::from_iter(defs),
    });
    tidy(&mut document, &regex::Regex::new(r"\[(`[^`]+`)\]").unwrap());
    document
}

/// Each description in `value` as a doc comment reads, not as it's wrapped
/// in the source: its lines joined, a paragraph apart from the next, and a
/// link, which `link` finds, as the name it links to.
#[cfg(test)]
fn tidy(value: &mut Value, link: &regex::Regex) {
    match value {
        Value::Object(object) => {
            for (key, child) in object.iter_mut() {
                match child {
                    Value::String(text) if key == "description" => {
                        let paragraphs: Vec<String> = text
                            .split("\n\n")
                            .map(|paragraph| {
                                let lines: Vec<&str> = paragraph.lines().map(str::trim).collect();
                                link.replace_all(&lines.join(" "), "$1").into_owned()
                            })
                            .collect();
                        *text = paragraphs.join("\n\n");
                    }
                    _ => tidy(child, link),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| tidy(item, link)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{self, Request, Response, SessionInfo};
    use serde_json::json;
    use std::collections::BTreeSet;

    /// Checks `value` against `schema`, strictly: a field of an object the
    /// schema doesn't name is wrong too, so the schema says all crystal
    /// writes. It knows the parts of JSON Schema schemars writes, and its
    /// errors say where in `value` they are.
    fn check(value: &Value, schema: &Value, root: &Value, at: &str) -> Result<(), String> {
        let named = names(value, schema, root, at)?;
        if let Value::Object(object) = value
            && let Some(field) = object.keys().find(|field| !named.contains(*field))
        {
            return Err(format!("{at}.{field} isn't in the schema"));
        }
        Ok(())
    }

    /// Checks `value` against `schema`, as [`check`] does but for the
    /// fields of an object at the top, and answers the fields the schema
    /// names there, through its `$ref`, `oneOf` and `anyOf` too.
    fn names(
        value: &Value,
        schema: &Value,
        root: &Value,
        at: &str,
    ) -> Result<BTreeSet<String>, String> {
        let schema = match schema {
            Value::Object(schema) => schema,
            Value::Bool(true) => return Ok(BTreeSet::new()),
            _ => return Err(format!("{at}: no schema takes it")),
        };
        let mut named = BTreeSet::new();
        if let Some(Value::String(reference)) = schema.get("$ref") {
            let pointer = reference.strip_prefix('#').unwrap();
            let target = root
                .pointer(pointer)
                .ok_or(format!("{reference} isn't there"))?;
            named.extend(names(value, target, root, at)?);
        }
        if let Some(types) = schema.get("type") {
            let types: Vec<&str> = match types {
                Value::Array(types) => types.iter().filter_map(Value::as_str).collect(),
                types => types.as_str().into_iter().collect(),
            };
            let is = |kind: &str| match kind {
                "null" => value.is_null(),
                "boolean" => value.is_boolean(),
                "integer" => value.is_i64() || value.is_u64(),
                "number" => value.is_number(),
                "string" => value.is_string(),
                "array" => value.is_array(),
                "object" => value.is_object(),
                _ => false,
            };
            if !types.iter().any(|kind| is(kind)) {
                return Err(format!("{at}: {value} isn't of type {types:?}"));
            }
        }
        if let Some(constant) = schema.get("const")
            && constant != value
        {
            return Err(format!("{at}: {value} isn't {constant}"));
        }
        if let Some(Value::Array(choices)) = schema.get("enum")
            && !choices.contains(value)
        {
            return Err(format!("{at}: {value} isn't one of {choices:?}"));
        }
        for keyword in ["oneOf", "anyOf"] {
            let Some(Value::Array(branches)) = schema.get(keyword) else {
                continue;
            };
            let tried: Vec<Result<BTreeSet<String>, String>> = branches
                .iter()
                .map(|branch| names(value, branch, root, at))
                .collect();
            let passed: Vec<&BTreeSet<String>> = tried.iter().flatten().collect();
            if passed.is_empty() || (keyword == "oneOf" && passed.len() > 1) {
                let errors: Vec<&String> = tried
                    .iter()
                    .filter_map(|tried| tried.as_ref().err())
                    .collect();
                return Err(format!(
                    "{at}: {} of {keyword} pass: {errors:#?}",
                    passed.len()
                ));
            }
            named.extend(passed.into_iter().flatten().cloned());
        }
        if let Value::Object(object) = value {
            let properties = schema.get("properties").and_then(Value::as_object);
            for (field, property) in properties.into_iter().flatten() {
                named.insert(field.clone());
                if let Some(value) = object.get(field) {
                    check(value, property, root, &format!("{at}.{field}"))?;
                }
            }
            for field in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let field = field.as_str().unwrap();
                if !object.contains_key(field) {
                    return Err(format!("{at}.{field} is missing"));
                }
            }
            if let Some(rest) = schema.get("additionalProperties") {
                for (field, value) in object {
                    if !properties.is_some_and(|properties| properties.contains_key(field)) {
                        check(value, rest, root, &format!("{at}.{field}"))?;
                        named.insert(field.clone());
                    }
                }
            }
        }
        if let Value::Array(items) = value {
            let prefix = schema.get("prefixItems").and_then(Value::as_array);
            let fixed = prefix.map_or(0, Vec::len);
            for (index, (item, schema)) in
                items.iter().zip(prefix.into_iter().flatten()).enumerate()
            {
                check(item, schema, root, &format!("{at}[{index}]"))?;
            }
            if let Some(schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate().skip(fixed) {
                    check(item, schema, root, &format!("{at}[{index}]"))?;
                }
            }
        }
        Ok(named)
    }

    /// Each `$ref` in `value`.
    fn refs<'a>(value: &'a Value, found: &mut Vec<&'a str>) {
        match value {
            Value::Object(object) => {
                if let Some(Value::String(reference)) = object.get("$ref") {
                    found.push(reference);
                }
                object.values().for_each(|child| refs(child, found));
            }
            Value::Array(items) => items.iter().for_each(|item| refs(item, found)),
            _ => {}
        }
    }

    #[test]
    fn api_schema_file_is_what_the_types_make() {
        let made = format!("{:#}\n", document());
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/crystal-api.schema.json");
        if std::env::var_os("CRYSTAL_UPDATE_API_SCHEMA").is_some() {
            std::fs::write(&path, &made).unwrap();
            return;
        }
        assert!(
            made == JSON,
            "{} isn't what the types make: run `CRYSTAL_UPDATE_API_SCHEMA=1 cargo test \
             api_schema` to write it again",
            path.display()
        );
    }

    #[test]
    fn every_ref_in_the_api_schema_finds_its_type() {
        let schema = document();
        let mut found = Vec::new();
        refs(&schema, &mut found);
        assert!(found.len() > 100, "{}", found.len());
        for reference in found {
            let name = reference.strip_prefix("#/$defs/").unwrap_or(reference);
            assert!(
                schema["$defs"].get(name).is_some(),
                "{reference} isn't in $defs"
            );
        }
    }

    #[test]
    fn no_two_types_share_a_name_in_the_api_schema() {
        // schemars tells two types of one name apart with a number, which
        // changes as they're found: give one a `#[schemars(rename)]`.
        let schema = document();
        for name in schema["$defs"].as_object().unwrap().keys() {
            assert!(
                !name.ends_with(|c: char| c.is_ascii_digit()),
                "{name}: another type has its name"
            );
        }
    }

    #[test]
    fn a_request_names_its_kinds_and_the_version_beside_them() {
        let schema = document();
        let request = &schema["schemas"]["request"];
        assert_eq!(request["$ref"], "#/$defs/Request");
        assert_eq!(request["properties"]["version"]["type"], "string");
        let kinds: Vec<&str> = schema["$defs"]["Request"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|kind| kind["properties"]["type"]["const"].as_str().unwrap())
            .collect();
        for kind in ["new", "list", "subscribe", "shutdown", "handover"] {
            assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
        }
    }

    #[test]
    fn every_event_kind_is_in_the_api_schema_with_when_it_happens() {
        let schema = document();
        let kinds = schema["$defs"]["EventKind"]["oneOf"].as_array().unwrap();
        assert_eq!(kinds.len(), crate::events::Kind::ALL.len());
        assert_eq!(kinds[0]["const"], "session.started");
        assert!(kinds.iter().all(|kind| kind["description"].is_string()));
    }

    /// `message` checked against the schema of the message called `name`.
    fn check_message(schema: &Value, name: &str, message: impl serde::Serialize) {
        let message = serde_json::to_value(message).unwrap();
        if let Err(error) = check(&message, &schema["schemas"][name], schema, name) {
            panic!("{error}\n{message:#}");
        }
    }

    /// A session as the daemon lists it.
    fn session() -> SessionInfo {
        serde_json::from_value(json!({
            "name": "reviewer",
            "id": "18daf82437703c98-0",
            "command": ["claude"],
            "cwd": "/code/app",
            "pid": 41210,
            "state": "running",
            "activity": "waiting",
            "worktree": {
                "project": "app",
                "project_path": "/code/app",
                "path": "/code/app",
                "main": true,
                "branch": "main",
            },
            "front": { "kind": "agent", "program": "claude", "name": "Claude Code" },
            "task": { "id": 12, "goal": "fix the login redirect", "accept": ["tests pass"] },
            "model": "opus",
        }))
        .unwrap()
    }

    #[test]
    fn an_example_of_every_event_is_what_the_api_schema_says() {
        let schema = document();
        let dir = Path::new("/code/app");
        for kind in crate::events::Kind::ALL {
            let mut event = crate::events::example(kind, Some(&session()), dir);
            event.seq = 4120;
            check_message(&schema, "event", event);
        }
    }

    #[test]
    fn requests_as_they_are_sent_are_what_the_api_schema_says() {
        let schema = document();
        let requests = [
            Request::List,
            Request::Kill {
                name: "reviewer".into(),
            },
            Request::Send {
                name: "reviewer".into(),
                text: "check the diff".into(),
                enter: true,
                from: None,
                force: false,
            },
            Request::Subscribe {
                filter: Default::default(),
                since: None,
            },
            Request::Shutdown {
                keep_sessions: true,
            },
        ];
        for request in requests {
            let mut wire = Vec::new();
            protocol::send_request(&mut wire, &request).unwrap();
            let message: Value = serde_json::from_slice(&wire).unwrap();
            check_message(&schema, "request", message);
        }
    }

    #[test]
    fn responses_and_a_snapshot_are_what_the_api_schema_says() {
        let schema = document();
        let responses = [
            Response::Sessions {
                sessions: vec![session()],
            },
            Response::Attached {
                name: "reviewer".into(),
                id: "18daf82437703c98-0".into(),
                running: true,
                size: (40, 120),
            },
            Response::Subscribed { seq: 4120 },
            Response::Error {
                message: "no session called nope".into(),
            },
            Response::Done,
        ];
        for response in responses {
            check_message(&schema, "response", response);
        }
        let snapshot = crate::api::Snapshot {
            version: "0.3.0".into(),
            socket: "/tmp/crystal-501/default.sock".into(),
            seq: 4120,
            sessions: vec![crate::api::Listed {
                status: session().status(),
                session: session(),
            }],
            layout: None,
            projects: Vec::new(),
            tasks: Vec::new(),
            flows: Vec::new(),
            archived: Vec::new(),
        };
        check_message(&schema, "snapshot", snapshot);
    }

    #[test]
    fn the_check_finds_a_field_the_api_schema_doesnt_name() {
        let schema = document();
        let mut request = serde_json::to_value(Request::List).unwrap();
        request["version"] = json!("0.3.0");
        let at = &schema["schemas"]["request"];
        assert_eq!(check(&request, at, &schema, "request"), Ok(()));
        request["teleport"] = json!(true);
        let error = check(&request, at, &schema, "request").unwrap_err();
        assert_eq!(error, "request.teleport isn't in the schema");
        let wrong = json!({ "type": "kill", "name": 7 });
        assert!(check(&wrong, at, &schema, "request").is_err());
    }

    #[test]
    fn the_summary_names_each_message_in_a_screenful() {
        let summary = summary().unwrap();
        for message in ["request", "response", "event", "snapshot"] {
            assert!(summary.contains(message), "{summary}");
        }
        assert!(summary.contains("kinds, by `type`"), "{summary}");
        assert!(summary.lines().count() < 15, "{summary}");
    }
}
