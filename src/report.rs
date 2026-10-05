//! `crystal report`: any agent, or a script wrapped around one, telling
//! crystal what it's doing and how to pick its session up again after a
//! restart. crystal knows Claude Code by its hooks and reads other agents
//! off their screens; an agent that reports for itself needs neither. Its
//! first report takes the session's status over, and it holds the session
//! until it lets go with `--release`, or leaves and the shell is back in
//! front.
//!
//! After a restart, the command it gave runs in the session's directory:
//! typed into the session's shell when the session runs one, or else in
//! place of the session's command. Typed into a shell, it has to read the
//! same in any of them, so it starts with a plain command name, and none of
//! its words holds a quote or a control character: quoted as
//! [`shell::quote`] does, the rest reads the same in sh, bash, zsh, fish
//! and the others.
//!
//! With `--line` and `--model`, an agent or a script puts a short line
//! under its session's row in the sidebar, or the model it runs on, as
//! herdr's `report-metadata` does: for the sidebar alone, which doesn't
//! take the status over. With `--display-agent`, the agent the row says is
//! in front; and with `--title`, `--state-label` and `--token`, what a row
//! laid out with `title`, `state` and `$name` shows (see
//! [`crate::tui::rows`]). `crystal project report --token` puts tokens on
//! a project the same way, for its heading's and its worktrees' rows. Each
//! stays until it's said again or taken off, or for as long as `--ttl`
//! gives it.
//!
//! Any report can say who sent it, with `--source`, and number it, with
//! `--seq`: one numbered no higher than the last from the same source came
//! late, and is passed over, so a hook that runs late can't put back what
//! its agent was doing before. What an agent is doing and what's on its
//! row are numbered apart, so one command can say both under one number.
//! A source that took the session over is the one that lets go of it.

use crate::client;
use crate::printable;
use crate::protocol::{Activity, AgentEvent, AgentReport, Metadata, Request, Response, RowReport};
use crate::shell;
use crate::typing;
use crate::work;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// The most words a resume command may have.
const MOST_WORDS: usize = 64;

/// The most bytes its words may take together.
const MOST_BYTES: usize = 8 * 1024;

/// The longest name an agent may give itself.
const LONGEST_AGENT: usize = 40;

/// The longest a line or a model put on a row may be, once it's tidied.
const LONGEST_SHOWN: usize = 80;

/// The longest a source's name may be.
const LONGEST_SOURCE: usize = 80;

/// The longest `--ttl` keeps what a report says.
pub const LONGEST_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// The most sources a session takes numbered reports from, in all its
/// life.
const MOST_SOURCES: usize = 32;

/// The most tokens a session or a project shows at once.
pub const MOST_TOKENS: usize = 32;

/// The longest a token's name may be.
const LONGEST_TOKEN: usize = 32;

/// The words of the statuses a row's `state` says, which `--state-label`
/// gives labels by: an agent's, then a program's, then how a session
/// ended or waits to start.
pub const STATE_WORDS: [&str; 9] = [
    "waiting", "working", "done", "open", "idle", "running", "ended", "failed", "starting",
];

/// Tells the daemon what the agent in the session called `name`, or the
/// one this runs in, says about itself: what it puts on its row, if
/// `metadata` puts anything there, then what it's doing. Both are sent by
/// `metadata`'s source, under its number.
pub fn run(
    socket: &Path,
    name: Option<String>,
    metadata: Metadata,
    report: Option<AgentReport>,
) -> Result<()> {
    let id = work::own_session(socket, &name, "which session it's about")?;
    let (source, seq) = (metadata.source.clone(), metadata.seq);
    check_source(source.as_deref())?;
    if metadata.shows() {
        let (id, name) = (id.clone(), name.clone());
        ask(socket, &Request::ReportMetadata { id, name, metadata })?;
    }
    if let Some(report) = report {
        let request = Request::ReportAgent {
            id,
            name,
            report,
            source,
            seq,
        };
        ask(socket, &request)?;
    }
    Ok(())
}

fn ask(socket: &Path, request: &Request) -> Result<()> {
    match client::ask(socket, request, false)? {
        Some(Response::Done) => Ok(()),
        Some(_) => bail!("the daemon answered something else"),
        None => bail!("no daemon is running on {}", socket.display()),
    }
}

/// Refuses a source's name that isn't a short word of letters, digits and
/// `:._-`.
fn check_source(source: Option<&str>) -> Result<()> {
    let Some(source) = source else {
        return Ok(());
    };
    let fits = |c: char| c.is_ascii_alphanumeric() || ":._-".contains(c);
    ensure!(
        !source.is_empty() && source.len() <= LONGEST_SOURCE && source.chars().all(fits),
        "a source is up to {LONGEST_SOURCE} letters, digits and `:._-`, like `indexer` or `ci:lint`"
    );
    Ok(())
}

/// What's on a session's row by `crystal report --line`, `--model` and the
/// rest, or on a project's by `crystal project report`, in the daemon: each
/// with when it goes, and the last number each source gave its reports, of
/// what's on the row and of what its agent is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shown {
    #[serde(default)]
    line: Option<Kept>,
    #[serde(default)]
    model: Option<Kept>,
    #[serde(default)]
    title: Option<Kept>,
    #[serde(default)]
    display_agent: Option<Kept>,
    #[serde(default)]
    state_labels: BTreeMap<String, Kept>,
    #[serde(default)]
    tokens: BTreeMap<String, Kept>,
    #[serde(default)]
    seqs: BTreeMap<String, u64>,
    /// Handed over by crystals since these were.
    #[serde(default)]
    status_seqs: BTreeMap<String, u64>,
}

/// Something said for a row, and when it goes, if it does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Kept {
    text: String,
    until: Option<SystemTime>,
}

impl Kept {
    fn at(&self, now: SystemTime) -> Option<&str> {
        let gone = self.until.is_some_and(|until| until <= now);
        (!gone).then_some(self.text.as_str())
    }
}

impl Shown {
    /// Takes what `metadata` says, at `now`. False when it came after a
    /// later report from the same source, and was passed over.
    pub fn take(&mut self, metadata: &Metadata, now: SystemTime) -> Result<bool> {
        ensure!(
            metadata.shows(),
            "say what to put on the row: --line, --model, --title, --display-agent, \
             --state-label or --token"
        );
        let mut labels = BTreeMap::new();
        for (word, label) in &metadata.state_labels {
            labels.insert(state_word(word)?, label);
        }
        for name in metadata.tokens.keys() {
            check_token(name)?;
        }
        let until = match metadata.ttl_secs {
            Some(secs) => {
                let ttl = Duration::from_secs(secs);
                ensure!(
                    secs > 0 && ttl <= LONGEST_TTL,
                    "a --ttl is from a second to a day"
                );
                Some(now + ttl)
            }
            None => None,
        };
        let kept = |text: &str| {
            let text = tidy(text);
            (!text.is_empty()).then_some(Kept { text, until })
        };
        let put = |map: &mut BTreeMap<String, Kept>, key: &str, text: &str| match kept(text) {
            Some(kept) => map.insert(key.to_string(), kept),
            None => map.remove(key),
        };
        // The tokens it comes to, those gone by now making room for new
        // ones, are checked before anything is taken.
        let mut tokens = self.tokens.clone();
        tokens.retain(|_, kept| kept.at(now).is_some());
        for (name, value) in &metadata.tokens {
            put(&mut tokens, name, value);
        }
        ensure!(
            tokens.len() <= MOST_TOKENS,
            "a row shows {MOST_TOKENS} tokens at most"
        );
        if !in_order(&mut self.seqs, metadata.source.as_deref(), metadata.seq)? {
            return Ok(false);
        }
        if let Some(line) = &metadata.line {
            self.line = kept(line);
        }
        if let Some(model) = &metadata.model {
            self.model = kept(model);
        }
        if let Some(title) = &metadata.title {
            self.title = kept(title);
        }
        if let Some(agent) = &metadata.display_agent {
            self.display_agent = kept(agent);
        }
        for (word, label) in labels {
            put(&mut self.state_labels, word, label);
        }
        self.tokens = tokens;
        Ok(true)
    }

    /// The rest of what's on the row at `now`, while each lasts.
    pub fn row(&self, now: SystemTime) -> RowReport {
        let live = |map: &BTreeMap<String, Kept>| {
            map.iter()
                .filter_map(|(key, kept)| Some((key.clone(), kept.at(now)?.to_string())))
                .collect()
        };
        RowReport {
            title: self
                .title
                .as_ref()
                .and_then(|kept| kept.at(now))
                .map(String::from),
            display_agent: (self.display_agent.as_ref())
                .and_then(|kept| kept.at(now))
                .map(String::from),
            state_labels: live(&self.state_labels),
            tokens: live(&self.tokens),
        }
    }

    /// The tokens on a project's rows at `now`, while each lasts.
    pub fn tokens(&self, now: SystemTime) -> BTreeMap<String, String> {
        self.row(now).tokens
    }

    /// Whether a report of what the agent is doing, numbered `seq` by
    /// `source`, comes in order: false when it came after a later one from
    /// the same source, and is to be passed over.
    pub fn status_in_order(&mut self, source: Option<&str>, seq: Option<u64>) -> Result<bool> {
        in_order(&mut self.status_seqs, source, seq)
    }

    /// The line on the row at `now`, while it lasts.
    pub fn line(&self, now: SystemTime) -> Option<&str> {
        self.line.as_ref()?.at(now)
    }

    /// The model reported at `now`, while it lasts.
    pub fn model(&self, now: SystemTime) -> Option<&str> {
        self.model.as_ref()?.at(now)
    }
}

/// Whether the report numbered `seq` by `source` comes after the last one
/// that source numbered, in `seqs`, which it then takes the place of. A
/// report with no number is always in order; a session takes numbered
/// reports from [`MOST_SOURCES`] sources at most.
fn in_order(
    seqs: &mut BTreeMap<String, u64>,
    source: Option<&str>,
    seq: Option<u64>,
) -> Result<bool> {
    check_source(source)?;
    let Some(seq) = seq else {
        return Ok(true);
    };
    let source = source.unwrap_or_default();
    match seqs.get(source) {
        Some(&last) if seq <= last => return Ok(false),
        None if seqs.len() >= MOST_SOURCES => {
            bail!("this session has taken numbered reports from {MOST_SOURCES} sources")
        }
        _ => {}
    }
    seqs.insert(source.to_string(), seq);
    Ok(true)
}

/// Refuses a token's name that isn't a short word of letters, digits, `_`
/// and `-`, starting with a letter: what a layout writes after its `$`.
pub fn check_token(name: &str) -> Result<()> {
    let fits = |c: char| c.is_ascii_alphanumeric() || "_-".contains(c);
    ensure!(
        name.starts_with(|c: char| c.is_ascii_alphabetic())
            && name.len() <= LONGEST_TOKEN
            && name.chars().all(fits),
        "a token's name is up to {LONGEST_TOKEN} letters, digits, `_` and `-`, starting with a \
         letter, like `load` or `ci_status`: not `{name}`"
    );
    Ok(())
}

/// The status a `--state-label` is for, by its word: one of
/// [`STATE_WORDS`], or `blocked`, which `crystal report` takes for
/// `waiting` too.
pub fn state_word(word: &str) -> Result<&'static str> {
    let word = word.trim().to_lowercase();
    let word = if word == "blocked" { "waiting" } else { &word };
    match STATE_WORDS.iter().find(|known| **known == word) {
        Some(known) => Ok(known),
        None => bail!(
            "a state label is for one of {}: not `{word}`",
            STATE_WORDS.join(", ")
        ),
    }
}

/// `text` as a row can show it: on one line, without control characters
/// (see [`printable`]), trimmed, and cut to [`LONGEST_SHOWN`] characters.
fn tidy(text: &str) -> String {
    let one_line = printable::line(text);
    one_line.trim().chars().take(LONGEST_SHOWN).collect()
}

/// Refuses a resume command that can't be typed into any shell and read
/// the same: one that doesn't start with a plain command name, has a quote
/// or a control character in it, or is too long.
pub fn check_resume(argv: &[String]) -> Result<()> {
    let Some(program) = argv.first() else {
        bail!("the resume command is empty");
    };
    ensure!(
        argv.len() <= MOST_WORDS,
        "the resume command has more than {MOST_WORDS} words"
    );
    ensure!(
        argv.iter().map(String::len).sum::<usize>() <= MOST_BYTES,
        "the resume command is longer than {MOST_BYTES} bytes"
    );
    ensure!(
        !argv.iter().any(|arg| arg.chars().any(char::is_control)),
        "the resume command has a control character in it"
    );
    ensure!(
        !argv.iter().any(|arg| arg.contains('\'')),
        "the resume command has a quote in it, which not every shell reads the same"
    );
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-.".contains(c);
    ensure!(
        !program.starts_with('-') && program.chars().all(plain),
        "the resume command has to start with a command's name, found on the PATH, not {program}"
    );
    Ok(())
}

/// The name an agent gave itself, once it's one word that fits in the
/// sidebar.
pub fn checked_agent(agent: String) -> Result<String> {
    let agent = agent.trim().to_string();
    ensure!(
        !agent.is_empty() && !agent.contains(|c: char| c.is_whitespace() || c.is_control()),
        "an agent's name is one word"
    );
    ensure!(
        agent.chars().count() <= LONGEST_AGENT,
        "an agent's name is at most {LONGEST_AGENT} characters"
    );
    Ok(agent)
}

/// What an agent saying it's doing `state` means, given what it was doing:
/// `idle` after working ends a turn, which a person may not have seen yet,
/// and otherwise only says it's at its prompt.
pub fn event(state: Activity, before: Option<Activity>) -> AgentEvent {
    match state {
        Activity::Working => AgentEvent::TurnStarted,
        Activity::Waiting => AgentEvent::Asking,
        Activity::Done => AgentEvent::TurnEnded,
        Activity::Idle if before == Some(Activity::Working) => AgentEvent::TurnEnded,
        Activity::Idle => AgentEvent::Started,
    }
}

/// What's typed into a shell to run `argv`: the words quoted where they
/// need it, then Enter.
pub fn typed(argv: &[String]) -> Vec<u8> {
    let words: Vec<String> = argv.iter().map(|arg| shell::quote(arg)).collect();
    let mut line = words.join(" ").into_bytes();
    line.extend_from_slice(typing::ENTER);
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn a_resume_command_starts_with_a_plain_name() {
        assert!(check_resume(&argv(&["pi", "--session", "a b", "--model=x"])).is_ok());
        assert!(check_resume(&argv(&["my-agent.sh", "resume"])).is_ok());
        for bad in [
            argv(&[]),
            argv(&["/usr/bin/pi"]),
            argv(&["./pi"]),
            argv(&["-pi"]),
            argv(&["pi agent"]),
            argv(&["pi", "it's"]),
            argv(&["pi", "two\nlines"]),
            argv(&["pi", "\u{1b}[31m"]),
        ] {
            assert!(check_resume(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_resume_command_is_kept_short() {
        assert!(check_resume(&vec!["pi".to_string(); MOST_WORDS]).is_ok());
        assert!(check_resume(&vec!["pi".to_string(); MOST_WORDS + 1]).is_err());
        assert!(check_resume(&argv(&["pi", &"x".repeat(MOST_BYTES)])).is_err());
    }

    #[test]
    fn an_agent_s_name_is_one_short_word() {
        assert_eq!(checked_agent(" pi ".into()).unwrap(), "pi");
        assert!(checked_agent("".into()).is_err());
        assert!(checked_agent("my agent".into()).is_err());
        assert!(checked_agent("x".repeat(LONGEST_AGENT + 1)).is_err());
    }

    #[test]
    fn idle_after_working_ends_a_turn_and_otherwise_rests() {
        use Activity::*;
        assert_eq!(event(Working, None), AgentEvent::TurnStarted);
        assert_eq!(event(Waiting, Some(Working)), AgentEvent::Asking);
        assert_eq!(event(Done, Some(Idle)), AgentEvent::TurnEnded);
        assert_eq!(event(Idle, Some(Working)), AgentEvent::TurnEnded);
        assert_eq!(event(Idle, Some(Waiting)), AgentEvent::Started);
        assert_eq!(event(Idle, None), AgentEvent::Started);
    }

    fn line(line: &str) -> Metadata {
        Metadata {
            line: Some(line.into()),
            ..Metadata::default()
        }
    }

    #[test]
    fn a_line_on_a_row_is_tidied_and_taken_off_when_empty() {
        let now = SystemTime::now();
        let mut shown = Shown::default();
        assert!(shown.take(&line("  indexing\n40%\u{1b} "), now).unwrap());
        assert_eq!(shown.line(now), Some("indexing 40%"));
        shown.take(&line(&"x".repeat(200)), now).unwrap();
        assert_eq!(shown.line(now).unwrap().len(), LONGEST_SHOWN);
        // Saying the model leaves the line as it was.
        let model = Metadata {
            model: Some("pi-large".into()),
            ..Metadata::default()
        };
        shown.take(&model, now).unwrap();
        assert!(shown.line(now).is_some());
        assert_eq!(shown.model(now), Some("pi-large"));
        shown.take(&line(" "), now).unwrap();
        assert_eq!(shown.line(now), None);
        assert!(shown.take(&Metadata::default(), now).is_err());
    }

    #[test]
    fn what_a_report_says_goes_once_its_time_is_up() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let mut shown = Shown::default();
        let mut timed = line("building");
        timed.ttl_secs = Some(30);
        shown.take(&timed, now).unwrap();
        assert_eq!(shown.line(now + Duration::from_secs(29)), Some("building"));
        assert_eq!(shown.line(now + Duration::from_secs(30)), None);
        timed.ttl_secs = Some(0);
        assert!(shown.take(&timed, now).is_err());
        timed.ttl_secs = Some(LONGEST_TTL.as_secs() + 1);
        assert!(shown.take(&timed, now).is_err());
    }

    #[test]
    fn a_report_that_comes_late_from_its_source_is_passed_over() {
        let now = SystemTime::now();
        let mut shown = Shown::default();
        let numbered = |text: &str, source: &str, seq| Metadata {
            source: Some(source.into()),
            seq: Some(seq),
            ..line(text)
        };
        assert!(shown.take(&numbered("two", "ci", 2), now).unwrap());
        assert!(!shown.take(&numbered("one", "ci", 1), now).unwrap());
        assert!(!shown.take(&numbered("two again", "ci", 2), now).unwrap());
        assert_eq!(shown.line(now), Some("two"));
        // Another source counts for itself; a report with no number always
        // counts.
        assert!(shown.take(&numbered("lint", "lint", 1), now).unwrap());
        assert!(shown.take(&line("plain"), now).unwrap());
        assert_eq!(shown.line(now), Some("plain"));
        assert!(shown.take(&numbered("bad", "a b", 9), now).is_err());
        for source in 2..MOST_SOURCES {
            shown
                .take(&numbered("x", &format!("s{source}"), 1), now)
                .unwrap();
        }
        assert!(shown.take(&numbered("x", "one-too-many", 1), now).is_err());
    }

    #[test]
    fn what_an_agent_is_doing_is_numbered_apart_from_its_row() {
        let now = SystemTime::now();
        let mut shown = Shown::default();
        let numbered = Metadata {
            source: Some("hook".into()),
            seq: Some(5),
            ..line("indexing")
        };
        // One command says both under one number.
        assert!(shown.take(&numbered, now).unwrap());
        assert!(shown.status_in_order(Some("hook"), Some(5)).unwrap());
        assert!(!shown.status_in_order(Some("hook"), Some(4)).unwrap());
        assert!(!shown.status_in_order(Some("hook"), Some(5)).unwrap());
        assert!(shown.status_in_order(Some("hook"), Some(6)).unwrap());
        assert!(shown.status_in_order(Some("hook"), None).unwrap());
        assert!(shown.status_in_order(None, Some(1)).unwrap());
        assert!(shown.status_in_order(Some("a b"), Some(1)).is_err());
    }

    #[test]
    fn tokens_are_put_on_and_taken_off_one_at_a_time() {
        let now = SystemTime::now();
        let mut shown = Shown::default();
        let tokens = |pairs: &[(&str, &str)]| Metadata {
            tokens: pairs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            ..Metadata::default()
        };
        shown
            .take(&tokens(&[("load", " 90 "), ("ci", "green")]), now)
            .unwrap();
        shown.take(&tokens(&[("ci", "")]), now).unwrap();
        let row = shown.row(now);
        assert_eq!(row.tokens.len(), 1);
        assert_eq!(row.tokens["load"], "90");
        assert!(shown.take(&tokens(&[("$load", "1")]), now).is_err());
        assert!(shown.take(&tokens(&[("9lives", "1")]), now).is_err());
        let many: Vec<(String, String)> = (0..MOST_TOKENS)
            .map(|n| (format!("t{n}"), "x".to_string()))
            .collect();
        let many: Vec<(&str, &str)> = many.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        assert!(shown.take(&tokens(&many), now).is_err());
        // What was there stays when a report is refused.
        assert_eq!(shown.row(now).tokens.len(), 1);
    }

    #[test]
    fn a_title_an_agent_and_state_labels_go_on_the_row() {
        let now = SystemTime::now();
        let mut shown = Shown::default();
        let said = Metadata {
            title: Some("refund fix".into()),
            display_agent: Some("pi".into()),
            state_labels: [("blocked".to_string(), "needs a key".to_string())].into(),
            ..Metadata::default()
        };
        shown.take(&said, now).unwrap();
        let row = shown.row(now);
        assert_eq!(row.title.as_deref(), Some("refund fix"));
        assert_eq!(row.display_agent.as_deref(), Some("pi"));
        assert_eq!(row.state_labels["waiting"], "needs a key");
        let unknown = Metadata {
            state_labels: [("asleep".to_string(), "zz".to_string())].into(),
            ..Metadata::default()
        };
        assert!(shown.take(&unknown, now).is_err());
        let off = Metadata {
            title: Some(String::new()),
            state_labels: [("waiting".to_string(), String::new())].into(),
            ..Metadata::default()
        };
        shown.take(&off, now).unwrap();
        let row = shown.row(now);
        assert_eq!(row.title, None);
        assert!(row.state_labels.is_empty());
        assert_eq!(row.display_agent.as_deref(), Some("pi"));
    }

    #[test]
    fn a_resume_command_is_typed_quoted_then_entered() {
        assert_eq!(
            typed(&argv(&["pi", "--session", "a b"])),
            b"pi --session 'a b'\r"
        );
    }
}
