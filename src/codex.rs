//! What crystal knows about OpenAI's Codex CLI: how to find the
//! conversation a Codex session is in, how to start Codex back in it, and
//! how to give it crystal's notes, as its developer instructions.
//!
//! Codex names its conversation in one place only: the file it records it
//! to, its "rollout", at `$CODEX_HOME/sessions/YYYY/MM/DD/` (`~/.codex`
//! without `CODEX_HOME`). The file is named after the local time Codex
//! started, like `rollout-2026-10-03T14-05-09-<id>.jsonl`, and its first
//! line describes the conversation: `{"type":"session_meta","payload":
//! {"id": ..., "cwd": ...}}`. Codex writes the file once there's something
//! to keep, so a session nobody has sent a prompt to has none, and nothing
//! to resume.
//!
//! Codex has hooks too, but crystal can't use them the way it uses Claude
//! Code's: they're only read from config files, never from the command
//! line, and Codex skips a hook the user hasn't reviewed. Its `notify`
//! command can be set from the command line, but that would replace the
//! user's own. So crystal reads what Codex is doing off its screen, and
//! finds its conversation from its rollout.

use crate::catalog::Instructions;
use crate::protocol::Conversation;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The setting Codex takes standing instructions in, which it adds to the
/// session as a developer message.
pub const INSTRUCTIONS: &str = "developer_instructions";

/// How long before the session started a rollout may say Codex started:
/// the names only count whole seconds.
const START_SLACK: Duration = Duration::from_secs(2);

/// How long after the session started Codex may have started its
/// conversation. A rollout started later is one the user began from
/// inside Codex, with `/new`, which crystal doesn't follow.
const START_WINDOW: Duration = Duration::from_secs(60);

/// How often to look for a session's rollout until it turns up.
const LOOK_EVERY: Duration = Duration::from_secs(1);

/// Codex's options that take a value, so that the word after one isn't
/// the first prompt. Any other option is taken to be a flag on its own.
const VALUE_OPTIONS: &[&str] = &[
    "-a",
    "--ask-for-approval",
    "-c",
    "--config",
    "-C",
    "--cd",
    "-i",
    "--image",
    "-m",
    "--model",
    "-p",
    "--profile",
    "-s",
    "--sandbox",
    "--add-dir",
    "--enable",
    "--disable",
    "--local-provider",
];

/// Options of `codex resume` that choose a conversation, which crystal is
/// choosing instead.
const RESUME_CHOOSERS: &[&str] = &["--last", "--all", "--include-non-interactive"];

/// Codex's subcommands. A command line that starts with one isn't an
/// interactive session, except `resume` and `fork`, which start one in a
/// conversation of its own.
const SUBCOMMANDS: &[&str] = &[
    "a",
    "agents",
    "app",
    "app-server",
    "apply",
    "archive",
    "cloud",
    "cloud-tasks",
    "completion",
    "debug",
    "delete",
    "doctor",
    "e",
    "exec",
    "exec-server",
    "execpolicy",
    "features",
    "fork",
    "login",
    "logout",
    "mcp",
    "migrate-rollouts",
    "plugin",
    "queue",
    "remote-control",
    "responses-api-proxy",
    "resume",
    "review",
    "sandbox",
    "stdio-to-uds",
    "tcp-tunnel",
    "unarchive",
    "update",
];

/// The command line that starts Codex back in conversation `id`, given the
/// one a session was started with: `codex resume <id>` with the same
/// options, but not the first prompt, which the conversation has already
/// had. `None` for a command line that isn't an interactive session, like
/// `codex exec`, which runs as asked.
pub fn resume_argv(command: &[String], id: &str) -> Option<Vec<String>> {
    let mut args = &command[1..];
    if let Some(first) = args.first().filter(|arg| !arg.starts_with('-')) {
        match first.as_str() {
            "resume" | "fork" => args = without_conversation(&args[1..]),
            word if SUBCOMMANDS.contains(&word) => return None,
            _ => {}
        }
    }
    let mut argv = vec![command[0].clone(), "resume".to_string(), id.to_string()];
    argv.extend(options_only(args));
    Some(argv)
}

/// The arguments of `codex resume` or `codex fork` after its subcommand,
/// without the id of the conversation it names, if it names one.
fn without_conversation(args: &[String]) -> &[String] {
    match args.first() {
        Some(first) if !first.starts_with('-') => &args[1..],
        _ => args,
    }
}

/// `args` with only the options left, and their values: no first prompt,
/// and nothing that chooses a conversation.
fn options_only(args: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            // Everything after it is the prompt.
            break;
        }
        if RESUME_CHOOSERS.contains(&arg.as_str()) {
            continue;
        }
        if !arg.starts_with('-') {
            // A word that isn't an option, nor an option's value: the
            // first prompt.
            continue;
        }
        kept.push(arg.clone());
        if VALUE_OPTIONS.contains(&arg.as_str())
            && let Some(value) = args.next()
        {
            kept.push(value.clone());
        }
    }
    kept
}

/// `$CODEX_HOME`, where Codex keeps its config and its sessions, for a
/// Codex run with `env`: `~/.codex` without it.
pub fn home(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    match env.get("CODEX_HOME") {
        Some(home) if !home.is_empty() => Some(PathBuf::from(home)),
        _ => Some(PathBuf::from(env.get("HOME")?).join(".codex")),
    }
}

/// A Codex command line with crystal's `instructions`, a paragraph each,
/// added to the developer instructions Codex would have had without them:
/// those given last with `-c`, like a profile's, which are taken out, or
/// else those in its config file in `home`. Codex keeps only the setting
/// given last, so crystal gives all of them in one, ahead of the rest. Any
/// other program's command line, or one with nothing to add, stays as it is.
pub fn with_instructions(
    argv: Vec<String>,
    instructions: &[String],
    home: Option<&Path>,
) -> Vec<String> {
    if instructions.is_empty() || crate::agents::program_name(&argv) != Some("codex") {
        return argv;
    }
    let (args, given) = without_instructions(&argv[1..]);
    let own = given.or_else(|| configured_instructions(home?));
    let text: Vec<String> = own.into_iter().chain(instructions.to_vec()).collect();
    let mut with = vec![argv[0].clone()];
    with.extend(Instructions::Setting(INSTRUCTIONS).args(&text.join("\n\n")));
    with.extend(args);
    with
}

/// Codex's arguments without the developer instructions given with `-c`,
/// and the text of the last of them, the one Codex would keep. Everything
/// after `--` is the first prompt, whatever it says.
fn without_instructions(args: &[String]) -> (Vec<String>, Option<String>) {
    let mut kept = Vec::new();
    let mut given = None;
    let mut at = 0;
    while at < args.len() {
        let arg = &args[at];
        if arg == "--" {
            kept.extend_from_slice(&args[at..]);
            break;
        }
        let (setting, taken) = match arg.strip_prefix("--config=") {
            Some(setting) => (Some(setting), 1),
            None if arg == "-c" || arg == "--config" => (args.get(at + 1).map(String::as_str), 2),
            None => (None, 1),
        };
        match setting.and_then(instructions_in) {
            Some(text) => given = Some(text),
            None => kept.extend_from_slice(&args[at..(at + taken).min(args.len())]),
        }
        at += taken;
    }
    (kept, given)
}

/// The text a `-c key=value` gives Codex's developer instructions, if
/// that's what it sets: the value read as TOML, or as it is when it isn't
/// TOML, the way Codex reads it.
fn instructions_in(setting: &str) -> Option<String> {
    let (key, value) = setting.split_once('=')?;
    if key.trim() != INSTRUCTIONS {
        return None;
    }
    let value = value.trim();
    match value.parse::<toml_edit::Value>() {
        Ok(toml_edit::Value::String(text)) => Some(text.into_value()),
        _ => Some(value.to_string()),
    }
}

/// The developer instructions in Codex's own config file, in `home`.
fn configured_instructions(home: &Path) -> Option<String> {
    let text = fs::read_to_string(home.join("config.toml")).ok()?;
    let config: toml::Table = toml::from_str(&text).ok()?;
    Some(config.get(INSTRUCTIONS)?.as_str()?.to_string())
}

/// Where a Codex session's conversation is to be found, looked for until
/// Codex writes it down.
///
/// A rollout is the session's when it's for the session's directory, and
/// its name says Codex started within a minute of the session, closer to
/// it than to any other Codex session started in that directory and still
/// looking. Codex writes the file only once there's something in it, so two
/// sessions started a little apart can see each other's rollout first; the
/// start time in its name, not the order the files appear in, says whose it
/// is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rollouts {
    /// `$CODEX_HOME`: where Codex keeps its sessions.
    home: PathBuf,
    /// The session's directory, links resolved, which its rollout names.
    cwd: PathBuf,
    /// When the session started.
    started: SystemTime,
    #[serde(skip, default = "Instant::now")]
    next_look: Instant,
}

impl Rollouts {
    /// Where to look for the conversation of a session running `command`
    /// in `cwd`, with environment `env`, which started just now. `None`
    /// unless it's Codex.
    pub fn for_session(
        command: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Option<Rollouts> {
        let program = Path::new(command.first()?).file_name()?.to_str()?;
        if program != "codex" {
            return None;
        }
        Some(Rollouts {
            home: home(env)?,
            cwd: canonical(cwd),
            started: SystemTime::now(),
            next_look: Instant::now(),
        })
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn started(&self) -> SystemTime {
        self.started
    }

    /// Looks for the conversation, unless it was looked for a moment ago.
    /// `claimed` are conversations other sessions are in already; `rivals`
    /// the start times of the other Codex sessions in this directory still
    /// looking for theirs.
    pub fn look(&mut self, claimed: &[&str], rivals: &[SystemTime]) -> Option<Conversation> {
        if Instant::now() < self.next_look {
            return None;
        }
        self.next_look = Instant::now() + LOOK_EVERY;
        self.find(claimed, rivals)
    }

    /// The conversation, if Codex has written it down yet: of the rollouts
    /// that are the session's, the one Codex started closest to it.
    pub fn find(&self, claimed: &[&str], rivals: &[SystemTime]) -> Option<Conversation> {
        let mut found: Option<(Duration, Conversation)> = None;
        for dir in self.day_dirs() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(named) = rollout_stamp(&path).and_then(stamp_time) else {
                    continue;
                };
                // The name counts whole seconds only, but that's enough to
                // pass over the files that can't be the session's without
                // reading them.
                if !self.in_window(named) {
                    continue;
                }
                let Some(meta) = read_meta(&path) else {
                    continue;
                };
                if canonical(&meta.cwd) != self.cwd || claimed.contains(&meta.id.as_str()) {
                    continue;
                }
                let codex_started = meta.started.unwrap_or(named);
                if !self.could_be_ours(codex_started, rivals) {
                    continue;
                }
                let apart = distance(codex_started, self.started);
                if found.as_ref().is_none_or(|(best, _)| apart < *best) {
                    let conversation = Conversation {
                        id: meta.id,
                        transcript: Some(path),
                        prompted: false,
                    };
                    found = Some((apart, conversation));
                }
            }
        }
        found.map(|(_, conversation)| conversation)
    }

    /// Whether Codex starting at `codex_started` could be this session's:
    /// within a minute after it started, and no closer to a rival's start.
    fn could_be_ours(&self, codex_started: SystemTime, rivals: &[SystemTime]) -> bool {
        if !self.in_window(codex_started) {
            return false;
        }
        let ours = distance(codex_started, self.started);
        !rivals
            .iter()
            .any(|rival| distance(codex_started, *rival) < ours)
    }

    /// Whether Codex starting at `codex_started` is within a minute after
    /// the session started, give or take the slack of whole seconds.
    fn in_window(&self, codex_started: SystemTime) -> bool {
        let Some(earliest) = self.started.checked_sub(START_SLACK) else {
            return false;
        };
        codex_started >= earliest && codex_started <= self.started + START_WINDOW
    }

    /// The directories the session's rollout can be in: the day it started,
    /// and the next, for a session started just before midnight.
    fn day_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for moment in [self.started, self.started + START_WINDOW] {
            let Some(stamp) = local_stamp(moment) else {
                continue;
            };
            // `2026-10-03T…` lives in `sessions/2026/10/03`.
            let dir = self
                .home
                .join("sessions")
                .join(&stamp[0..4])
                .join(&stamp[5..7])
                .join(&stamp[8..10]);
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }
}

/// What a rollout's first line says about its conversation.
#[derive(Debug, PartialEq, Eq)]
struct Meta {
    id: String,
    cwd: PathBuf,
    /// When Codex started the conversation, to the millisecond, which
    /// tells apart sessions started in the same second.
    started: Option<SystemTime>,
}

/// Reads the conversation's id and directory off a rollout's first line.
fn read_meta(path: &Path) -> Option<Meta> {
    let file = fs::File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file).read_line(&mut line).ok()?;
    parse_meta(&line)
}

fn parse_meta(line: &str) -> Option<Meta> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value["type"] != "session_meta" {
        return None;
    }
    let payload = &value["payload"];
    Some(Meta {
        id: payload["id"].as_str()?.to_string(),
        cwd: PathBuf::from(payload["cwd"].as_str()?),
        started: payload["timestamp"].as_str().and_then(utc_time),
    })
}

/// The local time a rollout's name says Codex started, like
/// `2026-10-03T14-05-09`, or `None` for a file that isn't a rollout.
fn rollout_stamp(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_prefix("rollout-")?;
    if !rest.ends_with(".jsonl") {
        return None;
    }
    rest.get(0..19)
}

/// `time` in local time, the way Codex names its rollouts:
/// `2026-10-03T14-05-09`.
fn local_stamp(time: SystemTime) -> Option<String> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let seconds = libc::time_t::try_from(seconds).ok()?;
    // SAFETY: an all-zero tm is a valid value for localtime_r to fill in.
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: localtime_r reads `seconds` and writes only `local`, both of
    // which live for the whole call.
    if unsafe { libc::localtime_r(&seconds, &mut local) }.is_null() {
        return None;
    }
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}",
        local.tm_year + 1900,
        local.tm_mon + 1,
        local.tm_mday,
        local.tm_hour,
        local.tm_min,
        local.tm_sec,
    ))
}

/// The moment a local time like `2026-10-03T14-05-09` stands for, the way
/// back from [`local_stamp`].
fn stamp_time(stamp: &str) -> Option<SystemTime> {
    let mut local = date_and_time(stamp)?;
    // Whether summer time applies is for mktime to work out.
    local.tm_isdst = -1;
    // SAFETY: mktime reads and normalizes only `local`, which lives for
    // the whole call.
    let seconds = unsafe { libc::mktime(&mut local) };
    let seconds = u64::try_from(seconds).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// The moment a UTC time like `2026-10-03T11:05:09.123Z` stands for: how
/// Codex writes when a conversation started.
fn utc_time(text: &str) -> Option<SystemTime> {
    let mut utc = date_and_time(text)?;
    // SAFETY: timegm reads and normalizes only `utc`, which lives for the
    // whole call.
    let seconds = unsafe { libc::timegm(&mut utc) };
    let seconds = u64::try_from(seconds).ok()?;
    let millis: u64 = text.get(20..23).and_then(|ms| ms.parse().ok()).unwrap_or(0);
    Some(UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_millis(millis))
}

/// The date and time at the start of `text`, where both shapes Codex
/// writes keep them: `2026-10-03T14-05-09` and `2026-10-03T11:05:09.123Z`.
fn date_and_time(text: &str) -> Option<libc::tm> {
    let number = |at: std::ops::Range<usize>| -> Option<i32> { text.get(at)?.parse().ok() };
    // SAFETY: an all-zero tm is a valid value to fill in.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = number(0..4)? - 1900;
    tm.tm_mon = number(5..7)? - 1;
    tm.tm_mday = number(8..10)?;
    tm.tm_hour = number(11..13)?;
    tm.tm_min = number(14..16)?;
    tm.tm_sec = number(17..19)?;
    Some(tm)
}

/// How far apart two moments are, whichever comes first.
fn distance(a: SystemTime, b: SystemTime) -> Duration {
    match a.duration_since(b) {
        Ok(after) => after,
        Err(before) => before.duration(),
    }
}

/// `path` with links resolved, so that `/tmp/x` and `/private/tmp/x` are
/// the same directory; as it is when it doesn't exist.
fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    fn resumed(args: &[&str]) -> Option<Vec<String>> {
        resume_argv(&command(args), "abc")
    }

    #[test]
    fn resuming_keeps_the_options_and_leaves_the_first_prompt_out() {
        assert_eq!(
            resumed(&["codex", "--model", "o4", "fix the tests", "--full-auto"]).unwrap(),
            ["codex", "resume", "abc", "--model", "o4", "--full-auto"]
        );
        assert_eq!(
            resumed(&["/opt/bin/codex"]).unwrap(),
            ["/opt/bin/codex", "resume", "abc"]
        );
    }

    #[test]
    fn an_option_value_isnt_taken_for_the_prompt() {
        assert_eq!(
            resumed(&["codex", "-c", "model=o4", "-s", "read-only", "--search"]).unwrap(),
            [
                "codex",
                "resume",
                "abc",
                "-c",
                "model=o4",
                "-s",
                "read-only",
                "--search"
            ]
        );
        assert_eq!(
            resumed(&["codex", "--", "--looks like an option"]).unwrap(),
            ["codex", "resume", "abc"]
        );
    }

    #[test]
    fn a_session_started_with_resume_or_fork_resumes_its_own_conversation() {
        assert_eq!(
            resumed(&["codex", "resume", "old", "--model", "o4"]).unwrap(),
            ["codex", "resume", "abc", "--model", "o4"]
        );
        assert_eq!(
            resumed(&["codex", "resume", "--last"]).unwrap(),
            ["codex", "resume", "abc"]
        );
        assert_eq!(
            resumed(&["codex", "fork", "old"]).unwrap(),
            ["codex", "resume", "abc"]
        );
    }

    #[test]
    fn other_subcommands_arent_resumed() {
        assert_eq!(resumed(&["codex", "exec", "fix it"]), None);
        assert_eq!(resumed(&["codex", "login"]), None);
    }

    /// The developer instructions a Codex command line gives, read the way
    /// Codex reads its `-c`, and the rest of it.
    fn instructions_of(argv: &[String]) -> (String, Vec<String>) {
        assert_eq!(argv[1], "-c");
        let setting: toml::Table = toml::from_str(&argv[2]).unwrap();
        let text = setting[INSTRUCTIONS].as_str().unwrap().to_string();
        (text, argv[3..].to_vec())
    }

    fn notes() -> Vec<String> {
        vec![
            "About crystal.".to_string(),
            "What was learned.".to_string(),
        ]
    }

    #[test]
    fn crystal_s_notes_follow_the_instructions_codex_is_given_on_its_command_line() {
        let asked = command(&[
            "codex",
            "-c",
            "developer_instructions=\"Be brief.\"",
            "--config=developer_instructions='Keep changes small.'",
            "-c",
            "model_reasoning_effort=high",
            "--",
            "-c is a flag",
        ]);
        let argv = with_instructions(asked, &notes(), None);
        let (text, rest) = instructions_of(&argv);
        // Codex keeps the last given, so crystal's follow that one.
        assert_eq!(
            text,
            "Keep changes small.\n\nAbout crystal.\n\nWhat was learned."
        );
        assert_eq!(
            rest,
            ["-c", "model_reasoning_effort=high", "--", "-c is a flag"]
        );

        // A value that isn't TOML is taken as it is, as Codex takes it.
        let asked = command(&["codex", "-c", "developer_instructions = Be brief."]);
        let (text, _) = instructions_of(&with_instructions(asked, &notes(), None));
        assert!(text.starts_with("Be brief.\n\nAbout crystal."), "{text}");
    }

    #[test]
    fn without_any_of_its_own_crystal_s_notes_follow_codex_s_config_file() {
        let home = tempfile::tempdir().unwrap();
        let asked = command(&["codex", "-m", "o4", "--", "fix it"]);
        let (text, rest) = instructions_of(&with_instructions(
            asked.clone(),
            &notes(),
            Some(home.path()),
        ));
        assert_eq!(text, "About crystal.\n\nWhat was learned.");
        assert_eq!(rest, ["-m", "o4", "--", "fix it"]);

        let config = "model = \"o4\"\ndeveloper_instructions = \"\"\"\nUse tabs.\"\"\"\n";
        fs::write(home.path().join("config.toml"), config).unwrap();
        let (text, _) = instructions_of(&with_instructions(asked, &notes(), Some(home.path())));
        assert_eq!(text, "Use tabs.\n\nAbout crystal.\n\nWhat was learned.");
    }

    #[test]
    fn with_nothing_to_add_or_another_program_the_command_line_stays_as_it_is() {
        let asked = command(&["codex", "-c", "developer_instructions=\"Be brief.\""]);
        assert_eq!(with_instructions(asked.clone(), &[], None), asked);
        let other = command(&["gemini", "-c", "developer_instructions=x"]);
        assert_eq!(with_instructions(other.clone(), &notes(), None), other);
    }

    #[test]
    fn codex_s_home_is_codex_home_or_under_home() {
        let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect()
        };
        assert_eq!(
            home(&env(&[("CODEX_HOME", "/c"), ("HOME", "/h")])),
            Some(PathBuf::from("/c"))
        );
        assert_eq!(
            home(&env(&[("CODEX_HOME", ""), ("HOME", "/h")])),
            Some(PathBuf::from("/h/.codex"))
        );
        assert_eq!(home(&env(&[])), None);
    }

    #[test]
    fn a_rollouts_first_line_names_its_conversation() {
        let line = r#"{"timestamp":"2026-10-03T11:05:09.123Z","type":"session_meta","payload":{"id":"5973b6c0-94b8","timestamp":"2026-10-03T11:05:09.123Z","cwd":"/code/app","originator":"codex_cli_rs","cli_version":"0.150.0"}}"#;
        // 2026-10-03T11:05:09Z is this many seconds after 1970 began.
        let started = UNIX_EPOCH + Duration::from_secs(1_791_025_509) + Duration::from_millis(123);
        assert_eq!(
            parse_meta(line),
            Some(Meta {
                id: "5973b6c0-94b8".into(),
                cwd: PathBuf::from("/code/app"),
                started: Some(started),
            })
        );
        assert_eq!(parse_meta(r#"{"type":"response_item","payload":{}}"#), None);
        assert_eq!(parse_meta("not json"), None);
    }

    #[test]
    fn the_millisecond_codex_started_tells_apart_sessions_started_in_one_second() {
        // Both rollouts' names say the same second; their first lines say
        // which session started each.
        let home = Home::new();
        let first = ten_minutes_ago() + Duration::from_millis(100);
        let second = first + Duration::from_millis(400);
        home.write_started(first + Duration::from_millis(50), "first-thread", &home.cwd);
        home.write_started(
            second + Duration::from_millis(50),
            "second-thread",
            &home.cwd,
        );

        let ids = |me: SystemTime, rival: SystemTime| {
            let rollouts = home.rollouts(me);
            found_id(&rollouts, &[], &[rival])
        };
        assert_eq!(ids(first, second), Some("first-thread".into()));
        assert_eq!(ids(second, first), Some("second-thread".into()));
    }

    #[test]
    fn a_rollouts_name_says_when_codex_started() {
        let path = Path::new("/h/sessions/2026/10/03/rollout-2026-10-03T14-05-09-5973b6c0.jsonl");
        assert_eq!(rollout_stamp(path), Some("2026-10-03T14-05-09"));
        assert_eq!(
            rollout_stamp(Path::new("rollout-2026-10-03T14-05-09-x.jsonl.zst")),
            None
        );
        assert_eq!(rollout_stamp(Path::new("history.jsonl")), None);
    }

    #[test]
    fn local_time_is_written_the_way_codex_names_files() {
        let stamp = local_stamp(SystemTime::now()).unwrap();
        assert_eq!(stamp.len(), 19);
        assert_eq!(&stamp[10..11], "T");
        assert_eq!(stamp.matches('-').count(), 4);
    }

    #[test]
    fn a_local_time_reads_back_as_the_moment_it_was_written_from() {
        let now = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
        assert_eq!(stamp_time(&local_stamp(now).unwrap()), Some(now));
        assert_eq!(stamp_time("not a time"), None);
    }

    /// A Codex home to write rollouts into, and the directory of the
    /// sessions that look for them.
    struct Home {
        dir: tempfile::TempDir,
        cwd: PathBuf,
    }

    impl Home {
        fn new() -> Home {
            let dir = tempfile::tempdir().unwrap();
            let cwd = dir.path().join("project");
            fs::create_dir(&cwd).unwrap();
            Home { dir, cwd }
        }

        fn rollouts(&self, started: SystemTime) -> Rollouts {
            let mut env = BTreeMap::new();
            env.insert(
                "CODEX_HOME".to_string(),
                self.dir.path().display().to_string(),
            );
            let mut rollouts =
                Rollouts::for_session(&command(&["codex"]), &self.cwd, &env).unwrap();
            rollouts.started = started;
            rollouts
        }

        /// Writes a rollout Codex started at `at`, for conversation `id` in
        /// `cwd`.
        fn write(&self, at: SystemTime, id: &str, cwd: &Path) {
            let payload = serde_json::json!({"id": id, "cwd": cwd});
            self.write_payload(at, id, payload);
        }

        /// The same, with the millisecond Codex started in the first line
        /// too, the way Codex writes it.
        fn write_started(&self, at: SystemTime, id: &str, cwd: &Path) {
            let payload = serde_json::json!({"id": id, "cwd": cwd, "timestamp": utc_text(at)});
            self.write_payload(at, id, payload);
        }

        fn write_payload(&self, at: SystemTime, id: &str, payload: Value) {
            let stamp = local_stamp(at).unwrap();
            let dir = self
                .dir
                .path()
                .join("sessions")
                .join(&stamp[0..4])
                .join(&stamp[5..7])
                .join(&stamp[8..10]);
            fs::create_dir_all(&dir).unwrap();
            let meta = serde_json::json!({"type": "session_meta", "payload": payload});
            let path = dir.join(format!("rollout-{stamp}-{id}.jsonl"));
            fs::write(path, format!("{meta}\n")).unwrap();
        }
    }

    /// `time` in UTC, to the millisecond: `2026-10-03T11:05:09.123Z`.
    fn utc_text(time: SystemTime) -> String {
        let since = time.duration_since(UNIX_EPOCH).unwrap();
        let seconds = since.as_secs() as libc::time_t;
        // SAFETY: as in local_stamp, with gmtime_r for UTC.
        let mut utc: libc::tm = unsafe { std::mem::zeroed() };
        unsafe { libc::gmtime_r(&seconds, &mut utc) };
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            utc.tm_year + 1900,
            utc.tm_mon + 1,
            utc.tm_mday,
            utc.tm_hour,
            utc.tm_min,
            utc.tm_sec,
            since.subsec_millis(),
        )
    }

    fn found_id(rollouts: &Rollouts, claimed: &[&str], rivals: &[SystemTime]) -> Option<String> {
        rollouts
            .find(claimed, rivals)
            .map(|conversation| conversation.id)
    }

    /// A moment ten minutes ago, on a whole second, as rollout names count.
    fn ten_minutes_ago() -> SystemTime {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        UNIX_EPOCH + Duration::from_secs(now.as_secs() - 600)
    }

    fn seconds(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn the_session_finds_the_rollout_codex_started_closest_to_it() {
        let home = Home::new();
        let started = ten_minutes_ago();
        let elsewhere = home.dir.path().to_path_buf();

        home.write(started - seconds(300), "before", &home.cwd);
        home.write(started + seconds(1), "other-dir", &elsewhere);
        home.write(started + seconds(3), "ours", &home.cwd);
        home.write(started + seconds(20), "later", &home.cwd);

        let rollouts = home.rollouts(started);
        assert_eq!(found_id(&rollouts, &[], &[]), Some("ours".into()));
        let transcript = rollouts.find(&[], &[]).unwrap().transcript.unwrap();
        let name = format!(
            "rollout-{}-ours.jsonl",
            local_stamp(started + seconds(3)).unwrap()
        );
        assert!(transcript.ends_with(name));
    }

    #[test]
    fn a_conversation_another_session_is_in_is_passed_over() {
        let home = Home::new();
        let started = ten_minutes_ago();
        home.write(started + seconds(1), "taken", &home.cwd);
        home.write(started + seconds(2), "free", &home.cwd);

        let rollouts = home.rollouts(started);
        assert_eq!(found_id(&rollouts, &["taken"], &[]), Some("free".into()));
    }

    #[test]
    fn a_rollout_closer_to_another_sessions_start_is_that_sessions() {
        // Two sessions started 20s apart in one directory, and only the
        // second has been sent a prompt, so only its rollout exists.
        let home = Home::new();
        let first = ten_minutes_ago();
        let second = first + seconds(20);
        home.write(second + seconds(1), "theirs", &home.cwd);

        let rollouts = home.rollouts(first);
        assert_eq!(found_id(&rollouts, &[], &[second]), None);
        // On its own, the first would have taken it.
        assert_eq!(found_id(&rollouts, &[], &[]), Some("theirs".into()));
    }

    #[test]
    fn a_conversation_begun_later_from_inside_codex_isnt_followed() {
        let home = Home::new();
        let started = ten_minutes_ago();
        home.write(started + seconds(120), "after-new", &home.cwd);
        assert_eq!(found_id(&home.rollouts(started), &[], &[]), None);
    }

    #[test]
    fn nothing_is_found_before_codex_writes_its_rollout() {
        let home = Home::new();
        assert_eq!(found_id(&home.rollouts(SystemTime::now()), &[], &[]), None);
    }

    #[test]
    fn only_codex_gets_its_rollouts_looked_for() {
        let env = BTreeMap::from([("HOME".to_string(), "/home/ann".to_string())]);
        let cwd = Path::new("/code");
        assert!(Rollouts::for_session(&command(&["claude"]), cwd, &env).is_none());
        let rollouts = Rollouts::for_session(&command(&["codex"]), cwd, &env).unwrap();
        assert_eq!(rollouts.home, Path::new("/home/ann/.codex"));
    }
}
