//! The rules crystal reads an agent's screen by: a file for each agent,
//! saying what on its screen means it's working, waiting on the user, or
//! settled at its prompt. crystal comes with one for each agent it knows,
//! the files in `agents/` (adapted from herdr's detection manifests), and a
//! file of the user's own in the `agents` directory beside the config file
//! takes the place of the one for the same agent, or adds an agent.
//!
//! A file names the agent: its `id`, what it's called, the other names its
//! program goes by and the npm packages it's run from. Then come its rules.
//! Each says what it means (`looks`: `working`, `waiting`, `settled`, or
//! `skip` for a screen that says nothing, like a transcript viewer, which
//! leaves the look as it was), how much it counts (`priority`: of the rules
//! that match, the highest wins, the first in the file on a tie), where it
//! looks (`region`), and what it looks for there: `contains` (text that
//! must all be there, in any case), `regex` (patterns that must all match),
//! `line_regex` (patterns each matching a line of its own), and the tests
//! within it: `all` must all pass, one of `any` must, and none of `not`
//! may. When no rule matches, the agent is settled.
//!
//! Agents change what they draw from one version to the next, which is why
//! the rules are files: a user can mend a rule before crystal ships a new
//! one, and `crystal agent explain` shows which rule read a session the way
//! it did.

use crate::agent_screen::Looks;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// The files crystal comes with, by the agent's id. `default` is for an
/// agent in front that has no file of its own.
const BUNDLED: &[(&str, &str)] = &[
    ("agy", include_str!("../agents/agy.toml")),
    ("amp", include_str!("../agents/amp.toml")),
    ("claude", include_str!("../agents/claude.toml")),
    ("cline", include_str!("../agents/cline.toml")),
    ("codex", include_str!("../agents/codex.toml")),
    ("copilot", include_str!("../agents/copilot.toml")),
    ("cursor", include_str!("../agents/cursor.toml")),
    ("devin", include_str!("../agents/devin.toml")),
    ("droid", include_str!("../agents/droid.toml")),
    ("gemini", include_str!("../agents/gemini.toml")),
    ("grok", include_str!("../agents/grok.toml")),
    ("hermes", include_str!("../agents/hermes.toml")),
    ("kilo", include_str!("../agents/kilo.toml")),
    ("kimi", include_str!("../agents/kimi.toml")),
    ("kiro", include_str!("../agents/kiro.toml")),
    ("letta", include_str!("../agents/letta.toml")),
    ("maki", include_str!("../agents/maki.toml")),
    ("mastracode", include_str!("../agents/mastracode.toml")),
    ("muse", include_str!("../agents/muse.toml")),
    ("opencode", include_str!("../agents/opencode.toml")),
    ("pi", include_str!("../agents/pi.toml")),
    ("qodercli", include_str!("../agents/qodercli.toml")),
    ("qwen", include_str!("../agents/qwen.toml")),
    (DEFAULT, include_str!("../agents/default.toml")),
];

/// The id of the rules for an agent with none of its own.
pub const DEFAULT: &str = "default";

/// How often the user's files are looked at again for changes, at most.
const RECHECK: Duration = Duration::from_secs(2);

/// Limits on a file, so that one can't make reading every screen slow.
const MOST_RULES: usize = 128;
const DEEPEST: usize = 8;
const MOST_TESTS: usize = 512;
const MOST_MATCHERS: usize = 1024;
const LONGEST_MATCHER: usize = 512;
/// The most memory one compiled pattern may take.
const REGEX_SIZE: usize = 1 << 20;

/// How much of a region `crystal agent explain` shows.
const SHOWN: usize = 240;

/// A rules file as it's written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    rules: Vec<RuleSpec>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleSpec {
    id: String,
    looks: Meaning,
    #[serde(default)]
    priority: i32,
    #[serde(default = "whole_screen")]
    region: String,
    #[serde(default)]
    all: Vec<TestSpec>,
    #[serde(default)]
    any: Vec<TestSpec>,
    #[serde(default)]
    not: Vec<TestSpec>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestSpec {
    #[serde(default)]
    all: Vec<TestSpec>,
    #[serde(default)]
    any: Vec<TestSpec>,
    #[serde(default)]
    not: Vec<TestSpec>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

fn whole_screen() -> String {
    "screen".to_string()
}

impl RuleSpec {
    /// The rule's own matchers and tests, as a test of their own.
    fn test(&self) -> TestSpec {
        TestSpec {
            all: self.all.iter().map(TestSpec::copy).collect(),
            any: self.any.iter().map(TestSpec::copy).collect(),
            not: self.not.iter().map(TestSpec::copy).collect(),
            contains: self.contains.clone(),
            regex: self.regex.clone(),
            line_regex: self.line_regex.clone(),
        }
    }
}

impl TestSpec {
    fn copy(&self) -> TestSpec {
        TestSpec {
            all: self.all.iter().map(TestSpec::copy).collect(),
            any: self.any.iter().map(TestSpec::copy).collect(),
            not: self.not.iter().map(TestSpec::copy).collect(),
            contains: self.contains.clone(),
            regex: self.regex.clone(),
            line_regex: self.line_regex.clone(),
        }
    }

    fn has_matcher(&self) -> bool {
        !self.contains.is_empty()
            || !self.regex.is_empty()
            || !self.line_regex.is_empty()
            || !self.all.is_empty()
            || !self.any.is_empty()
    }
}

/// What a rule says the screen means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Meaning {
    Working,
    Waiting,
    Settled,
    /// The screen says nothing about what the agent is doing, like a menu
    /// or a transcript viewer over its prompt: the look stays as it was.
    Skip,
}

impl Meaning {
    /// The look it means, or `None` for one to leave as it was.
    pub fn looks(self) -> Option<Looks> {
        match self {
            Meaning::Working => Some(Looks::Working),
            Meaning::Waiting => Some(Looks::Waiting),
            Meaning::Settled => Some(Looks::Settled),
            Meaning::Skip => None,
        }
    }
}

impl From<Looks> for Meaning {
    fn from(looks: Looks) -> Meaning {
        match looks {
            Looks::Working => Meaning::Working,
            Looks::Waiting => Meaning::Waiting,
            Looks::Settled => Meaning::Settled,
        }
    }
}

impl fmt::Display for Meaning {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Meaning::Working => "working",
            Meaning::Waiting => "waiting",
            Meaning::Settled => "settled",
            Meaning::Skip => "skip",
        })
    }
}

/// Where on the screen a rule looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// The title the program gave its terminal.
    Title,
    /// The progress it reports (OSC 9;4), as `4;1;-1`.
    Progress,
    /// The whole screen.
    Screen,
    /// The last rows with something on them.
    LastRows(usize),
    /// The first rows with something on them.
    FirstRows(usize),
    /// The rows after the last horizontal rule (`───`).
    AfterLastRule,
    /// Inside the box drawn around a prompt: between the last two rules.
    PromptBox,
    /// Everything above that box.
    AbovePromptBox,
    /// The last row with something on it above that box.
    LastRowAbovePromptBox,
    /// The rows after the last prompt line, one that is `›` or starts with
    /// `› `, as Codex draws its prompt.
    AfterLastPrompt,
    /// The rows before the prompt the user is at now: the last prompt line
    /// with no reply (`•`, `■`, `✗` or `✓`) after it.
    BeforeCurrentPrompt,
    /// The whole screen, unless the user is at a prompt now: then nothing.
    WithoutCurrentPrompt,
}

impl Region {
    fn parse(spec: &str) -> Result<Region, String> {
        let spec = spec.trim();
        let counted = |name: &str| -> Option<Result<usize, String>> {
            let count = spec
                .strip_prefix(name)?
                .strip_prefix('(')?
                .strip_suffix(')')?;
            Some(match count.parse::<usize>() {
                Ok(count) if (1..=usize::from(u16::MAX)).contains(&count) => Ok(count),
                _ => Err(format!(
                    "{name}() takes a number of rows from 1, not {count:?}"
                )),
            })
        };
        if let Some(count) = counted("last_rows") {
            return count.map(Region::LastRows);
        }
        if let Some(count) = counted("first_rows") {
            return count.map(Region::FirstRows);
        }
        Ok(match spec {
            "title" => Region::Title,
            "progress" => Region::Progress,
            "screen" => Region::Screen,
            "after_last_rule" => Region::AfterLastRule,
            "prompt_box" => Region::PromptBox,
            "above_prompt_box" => Region::AbovePromptBox,
            "last_row_above_prompt_box" => Region::LastRowAbovePromptBox,
            "after_last_prompt" => Region::AfterLastPrompt,
            "before_current_prompt" => Region::BeforeCurrentPrompt,
            "without_current_prompt" => Region::WithoutCurrentPrompt,
            _ => return Err(format!("there's no region {spec:?}")),
        })
    }

    /// The part of `screen` it covers.
    fn of<'a>(self, screen: &'a Input<'_>, text: &'a str) -> &'a str {
        match self {
            Region::Title => screen.title,
            Region::Progress => screen.progress,
            Region::Screen => text,
            Region::LastRows(count) => last_rows(text, count),
            Region::FirstRows(count) => first_rows(text, count),
            Region::AfterLastRule => after_last_rule(text),
            Region::PromptBox => prompt_box(text).unwrap_or(""),
            Region::AbovePromptBox => above_prompt_box(text),
            Region::LastRowAbovePromptBox => last_row(above_prompt_box(text)),
            Region::AfterLastPrompt => after_last_prompt(text),
            Region::BeforeCurrentPrompt => before_current_prompt(text),
            Region::WithoutCurrentPrompt => without_current_prompt(text),
        }
    }
}

impl fmt::Display for Region {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Region::Title => f.write_str("title"),
            Region::Progress => f.write_str("progress"),
            Region::Screen => f.write_str("screen"),
            Region::LastRows(count) => write!(f, "last_rows({count})"),
            Region::FirstRows(count) => write!(f, "first_rows({count})"),
            Region::AfterLastRule => f.write_str("after_last_rule"),
            Region::PromptBox => f.write_str("prompt_box"),
            Region::AbovePromptBox => f.write_str("above_prompt_box"),
            Region::LastRowAbovePromptBox => f.write_str("last_row_above_prompt_box"),
            Region::AfterLastPrompt => f.write_str("after_last_prompt"),
            Region::BeforeCurrentPrompt => f.write_str("before_current_prompt"),
            Region::WithoutCurrentPrompt => f.write_str("without_current_prompt"),
        }
    }
}

/// A rule, compiled.
#[derive(Debug)]
struct Rule {
    id: String,
    looks: Meaning,
    priority: i32,
    region: Region,
    test: Test,
}

/// A test, compiled: `contains` in lower case, the patterns made.
#[derive(Debug)]
struct Test {
    all: Vec<Test>,
    any: Vec<Test>,
    not: Vec<Test>,
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

impl Test {
    fn compile(spec: &TestSpec) -> Result<Test, String> {
        let compile_all =
            |specs: &[TestSpec]| specs.iter().map(Test::compile).collect::<Result<_, _>>();
        Ok(Test {
            all: compile_all(&spec.all)?,
            any: compile_all(&spec.any)?,
            not: compile_all(&spec.not)?,
            contains: spec
                .contains
                .iter()
                .map(|text| text.to_lowercase())
                .collect(),
            regex: patterns(&spec.regex)?,
            line_regex: patterns(&spec.line_regex)?,
        })
    }

    fn passes(&self, text: &str, lower: &str) -> bool {
        self.contains.iter().all(|wanted| lower.contains(wanted))
            && self.regex.iter().all(|regex| regex.is_match(text))
            && self
                .line_regex
                .iter()
                .all(|regex| text.lines().any(|line| regex.is_match(line)))
            && self.all.iter().all(|test| test.passes(text, lower))
            && (self.any.is_empty() || self.any.iter().any(|test| test.passes(text, lower)))
            && !self.not.iter().any(|test| test.passes(text, lower))
    }

    /// Why the test fails on `text`: the first thing it wants that isn't
    /// there. `None` when it passes.
    fn fails(&self, text: &str, lower: &str) -> Option<String> {
        if let Some(wanted) = self.contains.iter().find(|wanted| !lower.contains(*wanted)) {
            return Some(format!("no {wanted:?}"));
        }
        if let Some(regex) = self.regex.iter().find(|regex| !regex.is_match(text)) {
            return Some(format!("nothing matches /{}/", cut(regex.as_str())));
        }
        if let Some(regex) = self
            .line_regex
            .iter()
            .find(|regex| !text.lines().any(|line| regex.is_match(line)))
        {
            return Some(format!("no line matches /{}/", cut(regex.as_str())));
        }
        for (at, test) in self.all.iter().enumerate() {
            if let Some(why) = test.fails(text, lower) {
                return Some(format!("all[{at}]: {why}"));
            }
        }
        if !self.any.is_empty() && !self.any.iter().any(|test| test.passes(text, lower)) {
            let first = self.any[0].fails(text, lower).unwrap_or_default();
            return Some(format!("none of any[] passes (any[0]: {first})"));
        }
        if let Some(at) = self.not.iter().position(|test| test.passes(text, lower)) {
            return Some(format!("not[{at}] passes"));
        }
        None
    }
}

fn patterns(specs: &[String]) -> Result<Vec<Regex>, String> {
    specs
        .iter()
        .map(|spec| {
            RegexBuilder::new(spec)
                .size_limit(REGEX_SIZE)
                .build()
                .map_err(|err| format!("the pattern {spec:?} doesn't make sense: {err}"))
        })
        .collect()
}

/// Where an agent's rules come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// crystal's own.
    Bundled,
    /// The user's file, in place of crystal's.
    Override(PathBuf),
    /// The user's file, for an agent crystal has none for.
    Added(PathBuf),
}

impl Source {
    /// What `crystal agent` says it is: `bundled`, or the file's path.
    pub fn describe(&self) -> String {
        match self {
            Source::Bundled => "bundled".to_string(),
            Source::Override(path) | Source::Added(path) => crate::shell::home_relative(path),
        }
    }
}

/// One agent's rules: what it's called and how its screen is read.
#[derive(Debug, Clone)]
pub struct AgentRules {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub packages: Vec<String>,
    pub source: Source,
    /// A file of the user's for this agent that couldn't be used, and why:
    /// the rules before it stand in for it.
    pub problem: Option<String>,
    /// The rules as the file has them.
    specs: Arc<[RuleSpec]>,
    /// The rules compiled, the first time they're read with: a daemon only
    /// ever reads a few agents. In the order they're tried: the highest
    /// priority first, the first in the file first on a tie.
    rules: Arc<OnceLock<Vec<Rule>>>,
}

/// A screen to read: its rows, the title the program gave the terminal,
/// and the progress it reports.
pub struct Input<'a> {
    pub rows: &'a [String],
    pub title: &'a str,
    pub progress: &'a str,
}

impl Input<'_> {
    /// The rows as one text, a line each, ending in a line break, without
    /// the blank rows under what's been drawn.
    fn text(&self) -> String {
        let used = self
            .rows
            .iter()
            .rposition(|row| !row.trim().is_empty())
            .map_or(0, |last| last + 1);
        let mut text = String::new();
        for row in &self.rows[..used] {
            text.push_str(row.trim_end());
            text.push('\n');
        }
        text
    }
}

/// Why the rules read a screen the way they did: every rule, in the order
/// they're tried, with whether it matched and why not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explained {
    pub agent: String,
    pub name: String,
    /// `bundled`, or the user's file.
    pub source: String,
    #[serde(default)]
    pub problem: Option<String>,
    /// What the screen means. `skip` leaves the look as it was.
    pub looks: Meaning,
    /// The rule that said so; `None` when none matched, and so the agent
    /// is settled.
    #[serde(default)]
    pub decided_by: Option<String>,
    pub rules: Vec<Tried>,
}

/// A rule as it was tried on a screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tried {
    pub id: String,
    pub looks: Meaning,
    pub priority: i32,
    pub region: String,
    pub matched: bool,
    /// Why it didn't match: the first thing it wanted that wasn't there.
    #[serde(default)]
    pub why_not: Option<String>,
    /// The start of the text it looked at.
    pub seen: String,
}

/// Compiles rules as a file has them, in the order they're tried.
fn compile(specs: &[RuleSpec]) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::with_capacity(specs.len());
    for spec in specs {
        let region =
            Region::parse(&spec.region).map_err(|err| format!("rule {}: {err}", spec.id))?;
        let test = Test::compile(&spec.test()).map_err(|err| format!("rule {}: {err}", spec.id))?;
        rules.push(Rule {
            id: spec.id.clone(),
            looks: spec.looks,
            priority: spec.priority,
            region,
            test,
        });
    }
    // Stable, so the first in the file wins a tie.
    rules.sort_by_key(|rule| std::cmp::Reverse(rule.priority));
    Ok(rules)
}

impl AgentRules {
    fn new(file: File, source: Source) -> Result<AgentRules, String> {
        check(&file)?;
        Ok(AgentRules {
            name: file.name.unwrap_or_else(|| file.id.clone()),
            id: file.id,
            aliases: file.aliases,
            packages: file.packages,
            source,
            problem: None,
            specs: file.rules.into(),
            rules: Arc::default(),
        })
    }

    /// Compiles the rules now, rather than the first time they're read
    /// with, to find out whether they can be.
    fn compile_now(&self) -> Result<(), String> {
        if self.rules.get().is_none() {
            let _ = self.rules.set(compile(&self.specs)?);
        }
        Ok(())
    }

    /// The rules, compiled, in the order they're tried. Rules that can't
    /// be compiled are none: crystal's own always can, a test sees to it,
    /// and the user's are compiled as they're read.
    fn rules(&self) -> &[Rule] {
        self.rules
            .get_or_init(|| compile(&self.specs).unwrap_or_default())
    }

    /// What the screen means: a look, or `None` for one to leave as it
    /// was. Settled when no rule matches.
    pub fn read(&self, screen: &Input) -> Option<Looks> {
        let text = screen.text();
        for rule in self.rules() {
            let region = rule.region.of(screen, &text);
            if rule.test.passes(region, &region.to_lowercase()) {
                return rule.looks.looks();
            }
        }
        Some(Looks::Settled)
    }

    /// Every rule tried on `screen`, and what they made of it.
    pub fn explain(&self, screen: &Input) -> Explained {
        let text = screen.text();
        let mut decided: Option<&Rule> = None;
        let mut rules = Vec::with_capacity(self.specs.len());
        for rule in self.rules() {
            let region = rule.region.of(screen, &text);
            let lower = region.to_lowercase();
            let why_not = rule.test.fails(region, &lower);
            if why_not.is_none() && decided.is_none() {
                decided = Some(rule);
            }
            rules.push(Tried {
                id: rule.id.clone(),
                looks: rule.looks,
                priority: rule.priority,
                region: rule.region.to_string(),
                matched: why_not.is_none(),
                why_not,
                seen: shown(region),
            });
        }
        Explained {
            agent: self.id.clone(),
            name: self.name.clone(),
            source: self.source.describe(),
            problem: self.problem.clone(),
            looks: decided.map_or(Meaning::Settled, |rule| rule.looks),
            decided_by: decided.map(|rule| rule.id.clone()),
            rules,
        }
    }

    /// Whether `name`, a program's name, is this agent's.
    pub fn is_called(&self, name: &str) -> bool {
        self.id.eq_ignore_ascii_case(name)
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(name))
    }
}

/// A pattern as a reason shows it: its start, when it's long.
fn cut(pattern: &str) -> String {
    const LONGEST: usize = 48;
    if pattern.chars().count() <= LONGEST {
        return pattern.to_string();
    }
    let start: String = pattern.chars().take(LONGEST).collect();
    format!("{start}…")
}

/// The start of `text`, for showing.
fn shown(text: &str) -> String {
    let mut shown: String = text.chars().take(SHOWN).collect();
    if text.chars().count() > SHOWN {
        shown.push('…');
    }
    shown
}

/// Checks that a file is within the limits, and that each rule's tests can
/// match something.
fn check(file: &File) -> Result<(), String> {
    if file.id.trim().is_empty() {
        return Err("the file has no id".to_string());
    }
    if file.rules.is_empty() {
        return Err("the file has no rules".to_string());
    }
    if file.rules.len() > MOST_RULES {
        return Err(format!(
            "the file has {} rules, and the most is {MOST_RULES}",
            file.rules.len()
        ));
    }
    let mut count = Count::default();
    for rule in &file.rules {
        if rule.id.trim().is_empty() {
            return Err("a rule has no id".to_string());
        }
        let test = rule.test();
        if !test.has_matcher() {
            return Err(format!("rule {} looks for nothing", rule.id));
        }
        check_test(&test, 0, &mut count).map_err(|err| format!("rule {}: {err}", rule.id))?;
    }
    Ok(())
}

#[derive(Default)]
struct Count {
    tests: usize,
    matchers: usize,
}

fn check_test(test: &TestSpec, depth: usize, count: &mut Count) -> Result<(), String> {
    if depth > DEEPEST {
        return Err(format!("its tests go deeper than {DEEPEST}"));
    }
    count.tests += 1;
    if count.tests > MOST_TESTS {
        return Err(format!("the file has more than {MOST_TESTS} tests"));
    }
    let matchers = test
        .contains
        .iter()
        .chain(&test.regex)
        .chain(&test.line_regex);
    for matcher in matchers {
        count.matchers += 1;
        if matcher.chars().count() > LONGEST_MATCHER {
            return Err(format!(
                "a matcher is longer than {LONGEST_MATCHER} characters"
            ));
        }
    }
    if count.matchers > MOST_MATCHERS {
        return Err(format!("the file has more than {MOST_MATCHERS} matchers"));
    }
    for inner in test.all.iter().chain(&test.any) {
        if !inner.has_matcher() {
            return Err("a test in all or any looks for nothing".to_string());
        }
        check_test(inner, depth + 1, count)?;
    }
    for inner in &test.not {
        if !inner.has_matcher() && inner.not.is_empty() {
            return Err("a test in not looks for nothing".to_string());
        }
        check_test(inner, depth + 1, count)?;
    }
    Ok(())
}

/// Reads a rules file's text. A file of the user's is compiled as it's
/// read, so that one that can't be is found now; crystal's own the first
/// time they're used.
fn parse(text: &str, source: Source) -> Result<AgentRules, String> {
    let file: File = toml::from_str(text).map_err(|err| err.to_string())?;
    let rules = AgentRules::new(file, source)?;
    if rules.source != Source::Bundled {
        rules.compile_now()?;
    }
    Ok(rules)
}

/// crystal's own rules, read once.
fn bundled_agents() -> &'static [Arc<AgentRules>] {
    static AGENTS: OnceLock<Vec<Arc<AgentRules>>> = OnceLock::new();
    AGENTS.get_or_init(|| {
        let broken = |id: &str, err: String| AgentRules {
            id: id.to_string(),
            name: id.to_string(),
            aliases: Vec::new(),
            packages: Vec::new(),
            source: Source::Bundled,
            problem: Some(format!("crystal's own rules don't work: {err}")),
            specs: Arc::new([]),
            rules: Arc::default(),
        };
        BUNDLED
            .iter()
            .map(|(id, text)| parse(text, Source::Bundled).unwrap_or_else(|err| broken(id, err)))
            .map(Arc::new)
            .collect()
    })
}

/// The rules of every agent: crystal's own, with the user's files in their
/// place or beside them.
#[derive(Debug)]
pub struct Registry {
    agents: Vec<Arc<AgentRules>>,
    /// Files of the user's that couldn't be used for any agent, and why.
    pub problems: Vec<String>,
}

impl Registry {
    /// crystal's own rules, with the files in `dir`, when there is one.
    pub fn load(dir: Option<&Path>) -> Registry {
        let mut agents: Vec<Arc<AgentRules>> = bundled_agents().to_vec();
        let mut problems = Vec::new();
        for path in dir.map(files_in).unwrap_or_default() {
            let shown = crate::shell::home_relative(&path);
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) => {
                    problems.push(format!("couldn't read {shown}: {err}"));
                    continue;
                }
            };
            let stem = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned());
            match parse(&text, Source::Added(path.clone())) {
                Ok(mut rules) => {
                    let replaces = agents.iter().position(|agent| {
                        agent.is_called(&rules.id)
                            || rules.aliases.iter().any(|alias| agent.is_called(alias))
                    });
                    match replaces {
                        Some(at) if agents[at].source != Source::Bundled => problems.push(format!(
                            "{shown} has rules for {}, which {} has already",
                            agents[at].id,
                            agents[at].source.describe()
                        )),
                        Some(at) => {
                            rules.source = Source::Override(path.clone());
                            agents[at] = Arc::new(rules);
                        }
                        None => agents.push(Arc::new(rules)),
                    }
                }
                Err(err) => {
                    let why = format!("couldn't use {shown}: {err}");
                    // A broken file named for an agent says so on that
                    // agent, whose rules stand in for it.
                    let named =
                        stem.and_then(|stem| agents.iter().position(|a| a.is_called(&stem)));
                    match named {
                        Some(at) => {
                            let mut said = (*agents[at]).clone();
                            said.problem = Some(format!("{why}; its rules before it are used"));
                            agents[at] = Arc::new(said);
                        }
                        None => problems.push(why),
                    }
                }
            }
        }
        Registry { agents, problems }
    }

    /// Every agent's rules, `default` last.
    pub fn agents(&self) -> Vec<&Arc<AgentRules>> {
        let mut agents: Vec<&Arc<AgentRules>> = self.agents.iter().collect();
        agents.sort_by_key(|agent| (agent.id == DEFAULT, agent.id.clone()));
        agents
    }

    /// The agent `name`, a program's name or an agent's id, is, if there's
    /// a file for it.
    pub fn find(&self, name: &str) -> Option<&Arc<AgentRules>> {
        self.agents
            .iter()
            .find(|agent| agent.id != DEFAULT && agent.is_called(name))
    }

    /// The rules to read the agent `program` runs by: its own, or else the
    /// ones for any agent.
    pub fn for_program(&self, program: &str) -> &Arc<AgentRules> {
        self.find(program).unwrap_or_else(|| {
            self.agents
                .iter()
                .find(|agent| agent.id == DEFAULT)
                .expect("the default rules are always there")
        })
    }

    /// The agent run from `script`, a path inside one of its npm packages.
    pub fn by_script(&self, script: &str) -> Option<&Arc<AgentRules>> {
        self.agents.iter().find(|agent| {
            agent
                .packages
                .iter()
                .any(|package| script.contains(&format!("node_modules/{package}/")))
        })
    }

    /// Whether `a` and `b`, programs' names or agents' ids, are the same
    /// agent.
    pub fn same_agent(&self, a: &str, b: &str) -> bool {
        a.eq_ignore_ascii_case(b)
            || matches!((self.find(a), self.find(b)), (Some(a), Some(b)) if a.id == b.id)
    }

    /// Everything wrong with the user's files.
    pub fn all_problems(&self) -> Vec<String> {
        let mut all = self.problems.clone();
        all.extend(self.agents.iter().filter_map(|agent| agent.problem.clone()));
        all
    }
}

/// The `.toml` files in `dir`, by name.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    files
}

/// Where the user's files go: `agents` beside the config file.
pub fn dir() -> PathBuf {
    crate::config::path()
        .parent()
        .map(|config| config.join("agents"))
        .unwrap_or_else(|| PathBuf::from("agents"))
}

/// The text of the file crystal comes with for `agent`, an id or another
/// name of one.
pub fn bundled(agent: &str) -> Option<&'static str> {
    let id = Registry::load(None)
        .find(agent)
        .map(|rules| rules.id.clone())?;
    BUNDLED
        .iter()
        .find(|(bundled, _)| *bundled == id)
        .map(|(_, text)| *text)
}

/// What [`current`] keeps between calls.
struct Cache {
    registry: Arc<Registry>,
    /// The user's files as they were when they were read: path, when each
    /// changed, and how long it was.
    stamp: Vec<(PathBuf, Option<SystemTime>, u64)>,
    checked: Instant,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
static LOG_PROBLEMS: AtomicBool = AtomicBool::new(false);

/// Has [`current`] say on standard error what's wrong with the user's files
/// each time it reads them: the daemon's log.
pub fn log_problems() {
    LOG_PROBLEMS.store(true, Ordering::Relaxed);
    current();
}

/// The rules in effect: crystal's own with the user's files, read again
/// when the files have changed, which is looked at every few seconds at
/// most.
pub fn current() -> Arc<Registry> {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = cache.as_mut() {
        if cached.checked.elapsed() < RECHECK {
            return cached.registry.clone();
        }
        cached.checked = Instant::now();
    }
    let dir = user_dir();
    let stamp = stamp(dir.as_deref());
    if let Some(cached) = cache.as_ref()
        && cached.stamp == stamp
    {
        return cached.registry.clone();
    }
    let registry = Arc::new(Registry::load(dir.as_deref()));
    if LOG_PROBLEMS.load(Ordering::Relaxed) {
        for problem in registry.all_problems() {
            eprintln!("crystal daemon: agent rules: {problem}");
        }
    }
    *cache = Some(Cache {
        registry: registry.clone(),
        stamp,
        checked: Instant::now(),
    });
    registry
}

/// The user's directory of rules. Unit tests read crystal's own rules only,
/// whatever the machine they run on has.
fn user_dir() -> Option<PathBuf> {
    if cfg!(test) { None } else { Some(dir()) }
}

fn stamp(dir: Option<&Path>) -> Vec<(PathBuf, Option<SystemTime>, u64)> {
    dir.map(files_in)
        .unwrap_or_default()
        .into_iter()
        .map(|path| {
            let meta = std::fs::metadata(&path).ok();
            let changed = meta.as_ref().and_then(|meta| meta.modified().ok());
            let len = meta.map_or(0, |meta| meta.len());
            (path, changed, len)
        })
        .collect()
}

/// crystal's own rules, compiled once.
pub fn bundled_registry() -> &'static Registry {
    static BUNDLED_REGISTRY: OnceLock<Registry> = OnceLock::new();
    BUNDLED_REGISTRY.get_or_init(|| Registry::load(None))
}

// The regions, adapted from herdr's.

fn lines_of(text: &str) -> Vec<&str> {
    text.lines().collect()
}

/// Where the line at `index` of `lines`, `text`'s lines, starts in `text`.
fn line_start(text: &str, lines: &[&str], index: usize) -> usize {
    lines[..index.min(lines.len())]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>()
        .min(text.len())
}

fn from_line<'a>(text: &'a str, lines: &[&str], index: usize) -> &'a str {
    &text[line_start(text, lines, index)..]
}

fn last_rows(text: &str, count: usize) -> &str {
    let lines = lines_of(text);
    let first = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index);
    match first {
        Some(first) => from_line(text, &lines, first),
        None => "",
    }
}

fn first_rows(text: &str, count: usize) -> &str {
    let lines = lines_of(text);
    let last = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index);
    match last {
        Some(last) => &text[..line_start(text, &lines, last + 1)],
        None => "",
    }
}

/// A row that's a horizontal rule: `─` all the way, or three or more of
/// them before other text, like a rule with a label in it.
fn is_rule(line: &str) -> bool {
    let line = line.trim();
    let dashes = line.chars().take_while(|&c| c == '─').count();
    if dashes == 0 {
        return false;
    }
    let rest = line
        .char_indices()
        .nth(dashes)
        .map_or("", |(at, _)| &line[at..]);
    rest.trim_start().is_empty() || dashes >= 3
}

fn after_last_rule(text: &str) -> &str {
    let lines = lines_of(text);
    match lines.iter().rposition(|line| is_rule(line)) {
        Some(at) => from_line(text, &lines, at + 1),
        None => text,
    }
}

/// The top of the box around a prompt: the second rule from the bottom.
fn prompt_box_top(lines: &[&str]) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| is_rule(line))
        .nth(1)
        .map(|(at, _)| at)
}

fn prompt_box(text: &str) -> Option<&str> {
    let lines = lines_of(text);
    let top = prompt_box_top(&lines)?;
    let start = line_start(text, &lines, top + 1);
    let end = lines[top + 1..]
        .iter()
        .position(|line| is_rule(line))
        .map_or(lines.len(), |below| top + 1 + below);
    Some(&text[start..line_start(text, &lines, end).max(start)])
}

fn above_prompt_box(text: &str) -> &str {
    let lines = lines_of(text);
    match prompt_box_top(&lines) {
        Some(top) => &text[..line_start(text, &lines, top)],
        None => text,
    }
}

fn last_row(text: &str) -> &str {
    text.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
}

/// A prompt line, as Codex draws it: `›`, or `› ` and what's typed.
fn is_prompt(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

/// The start of a reply, as Codex draws it.
fn is_reply(line: &str) -> bool {
    line.starts_with(['•', '■', '✗', '✓'])
}

/// The prompt the user is at now: the last prompt line, when no reply
/// comes after it.
fn current_prompt(lines: &[&str]) -> Option<usize> {
    let at = lines.iter().rposition(|line| is_prompt(line))?;
    (!lines[at + 1..].iter().any(|line| is_reply(line))).then_some(at)
}

fn after_last_prompt(text: &str) -> &str {
    let lines = lines_of(text);
    match lines.iter().rposition(|line| is_prompt(line)) {
        Some(at) => from_line(text, &lines, at + 1),
        None => text,
    }
}

fn before_current_prompt(text: &str) -> &str {
    let lines = lines_of(text);
    match current_prompt(&lines) {
        Some(at) => &text[..line_start(text, &lines, at)],
        None => text,
    }
}

fn without_current_prompt(text: &str) -> &str {
    if current_prompt(&lines_of(text)).is_some() {
        ""
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    /// How `agent`'s own rules read a screen of `text`.
    fn read(agent: &str, text: &str, title: &str, progress: &str) -> Option<Looks> {
        let rows = rows(text);
        let screen = Input {
            rows: &rows,
            title,
            progress,
        };
        let registry = bundled_registry();
        let rules = registry
            .find(agent)
            .unwrap_or_else(|| panic!("no rules for {agent}"));
        rules.read(&screen)
    }

    fn rules_from(text: &str) -> AgentRules {
        parse(text, Source::Added(PathBuf::from("x.toml"))).unwrap()
    }

    fn on(rules: &AgentRules, text: &str) -> Option<Looks> {
        let rows = rows(text);
        rules.read(&Input {
            rows: &rows,
            title: "",
            progress: "",
        })
    }

    #[test]
    fn crystal_s_own_rules_all_work() {
        let registry = Registry::load(None);
        assert_eq!(registry.all_problems(), Vec::<String>::new());
        let mut ids: Vec<&str> = BUNDLED.iter().map(|(id, _)| *id).collect();
        for agent in registry.agents() {
            assert!(!agent.specs.is_empty(), "{} has no rules", agent.id);
            assert_eq!(agent.compile_now(), Ok(()), "{}", agent.id);
            assert_eq!(agent.source, Source::Bundled);
            assert_ne!(agent.name, agent.id, "{} has no name", agent.id);
        }
        // Each file is under the name of the agent it's for.
        let found: Vec<&str> = registry.agents().iter().map(|a| a.id.as_str()).collect();
        ids.sort_by_key(|id| (*id == DEFAULT, *id));
        assert_eq!(found, ids);
    }

    #[test]
    fn every_agent_crystal_offers_has_rules_or_the_common_ones() {
        let registry = bundled_registry();
        for agent in crate::catalog::AGENTS {
            let rules = registry.for_program(agent.program);
            let own = registry.find(agent.program).is_some();
            assert!(
                own || agent.program == "aider",
                "{} has no rules",
                agent.program
            );
            assert!(!rules.specs.is_empty());
        }
    }

    #[test]
    fn an_agent_is_found_by_any_of_its_names() {
        let registry = bundled_registry();
        assert_eq!(registry.find("cursor-agent").unwrap().id, "cursor");
        assert_eq!(registry.find("Claude").unwrap().id, "claude");
        assert_eq!(registry.find("kiro-cli").unwrap().id, "kiro");
        assert!(registry.find("vim").is_none());
        assert!(registry.find(DEFAULT).is_none(), "default is no agent");
        assert_eq!(registry.for_program("vim").id, DEFAULT);
        assert!(registry.same_agent("cursor", "cursor-agent"));
        assert!(!registry.same_agent("codex", "claude"));
        let script = "/usr/lib/node_modules/@qwen-code/qwen-code/dist/index.js";
        assert_eq!(registry.by_script(script).unwrap().id, "qwen");
        assert!(
            registry
                .by_script("/x/node_modules/vite/bin/vite.js")
                .is_none()
        );
    }

    #[test]
    fn the_highest_priority_match_wins_and_the_first_on_a_tie() {
        let rules = rules_from(
            r#"
            id = "x"
            [[rules]]
            id = "low"
            looks = "settled"
            priority = 1
            contains = ["match"]

            [[rules]]
            id = "high"
            looks = "working"
            priority = 10
            contains = ["match"]
            all = [{ any = [{ regex = ["w[io]n"] }, { contains = ["fallback"] }] }]
            not = [{ contains = ["blocked"] }]

            [[rules]]
            id = "first_of_two"
            looks = "waiting"
            priority = 5
            line_regex = ["^exact line$"]

            [[rules]]
            id = "second_of_two"
            looks = "working"
            priority = 5
            contains = ["exact"]
            "#,
        );
        assert_eq!(on(&rules, "match win"), Some(Looks::Working));
        assert_eq!(
            on(&rules, "MATCH FALLBACK"),
            Some(Looks::Working),
            "any case"
        );
        assert_eq!(on(&rules, "match win blocked"), Some(Looks::Settled));
        assert_eq!(on(&rules, "exact line"), Some(Looks::Waiting));
        assert_eq!(on(&rules, "not an exact line"), Some(Looks::Working));
        assert_eq!(on(&rules, "nothing"), Some(Looks::Settled));
    }

    #[test]
    fn a_skip_rule_leaves_the_look() {
        let rules = rules_from(
            r#"
            id = "x"
            [[rules]]
            id = "menu"
            looks = "skip"
            contains = ["menu"]
            "#,
        );
        assert_eq!(on(&rules, "a menu"), None);
        assert_eq!(on(&rules, "a prompt"), Some(Looks::Settled));
    }

    #[test]
    fn each_region_covers_its_part_of_the_screen() {
        let text = "top\n\nmiddle\n────────\nin the box\n────────\nbelow\n";
        assert_eq!(last_rows(text, 2), "────────\nbelow\n");
        assert_eq!(first_rows(text, 2), "top\n\nmiddle\n");
        assert_eq!(after_last_rule(text), "below\n");
        assert_eq!(prompt_box(text), Some("in the box\n"));
        assert_eq!(above_prompt_box(text), "top\n\nmiddle\n");
        assert_eq!(last_row(above_prompt_box(text)), "middle");
        assert_eq!(after_last_rule("no rule\n"), "no rule\n");
        assert_eq!(prompt_box("one rule\n───\n"), None);
        assert_eq!(last_rows("", 3), "");

        let codex = "• Ran it\n› old\n• Working (3s)\n› typing\n  model · dir\n";
        assert_eq!(after_last_prompt(codex), "  model · dir\n");
        assert_eq!(
            before_current_prompt(codex),
            "• Ran it\n› old\n• Working (3s)\n"
        );
        assert_eq!(without_current_prompt(codex), "");
        let replied = "› asked\n• answered\n";
        assert_eq!(before_current_prompt(replied), replied);
        assert_eq!(without_current_prompt(replied), replied);
    }

    #[test]
    fn a_region_s_rows_are_its_own() {
        // Text above the last rows doesn't count for a rule that looks
        // at them, nor does text in the title for one that looks at rows.
        let rules = rules_from(
            r#"
            id = "x"
            [[rules]]
            id = "bottom"
            looks = "working"
            region = "last_rows(1)"
            contains = ["busy"]

            [[rules]]
            id = "title"
            looks = "waiting"
            region = "title"
            contains = ["asking"]
            "#,
        );
        assert_eq!(on(&rules, "busy\n> "), Some(Looks::Settled));
        assert_eq!(on(&rules, "> \nbusy"), Some(Looks::Working));
        assert_eq!(on(&rules, "asking"), Some(Looks::Settled));
    }

    #[test]
    fn a_file_that_doesnt_make_sense_says_why() {
        let problem = |text: &str| parse(text, Source::Added(PathBuf::from("x.toml"))).unwrap_err();
        assert!(problem("id = \"x\"").contains("no rules"));
        let unknown = problem(
            "id = \"x\"\n[[rules]]\nid = \"r\"\nlooks = \"working\"\nstate = \"idle\"\ncontains = [\"a\"]",
        );
        assert!(unknown.contains("unknown field `state`"), "{unknown}");
        let looks =
            problem("id = \"x\"\n[[rules]]\nid = \"r\"\nlooks = \"blocked\"\ncontains = [\"a\"]");
        assert!(looks.contains("blocked"), "{looks}");
        let region = problem(
            "id = \"x\"\n[[rules]]\nid = \"r\"\nlooks = \"working\"\nregion = \"bottom\"\ncontains = [\"a\"]",
        );
        assert_eq!(region, "rule r: there's no region \"bottom\"");
        let rows = problem(
            "id = \"x\"\n[[rules]]\nid = \"r\"\nlooks = \"working\"\nregion = \"last_rows(0)\"\ncontains = [\"a\"]",
        );
        assert!(rows.contains("from 1"), "{rows}");
        let pattern =
            problem("id = \"x\"\n[[rules]]\nid = \"r\"\nlooks = \"working\"\nregex = [\"(\"]");
        assert!(
            pattern.starts_with("rule r: the pattern \"(\" doesn't make sense"),
            "{pattern}"
        );
        let empty = problem("id = \"x\"\n[[rules]]\nid = \"r\"\nlooks = \"working\"");
        assert_eq!(empty, "rule r looks for nothing");
        let deep = format!(
            "id = \"x\"\n[[rules]]\nid = \"r\"\nlooks = \"working\"\nall = [{}{{ contains = [\"a\"] }}{}]",
            "{ all = [".repeat(9),
            "] }".repeat(9)
        );
        assert!(
            problem(&deep).contains("deeper than 8"),
            "{}",
            problem(&deep)
        );
    }

    #[test]
    fn a_users_file_takes_the_place_of_crystal_s_or_adds_an_agent() {
        let dir = tempfile::tempdir().unwrap();
        let own =
            "id = \"codex\"\n[[rules]]\nid = \"mine\"\nlooks = \"waiting\"\ncontains = [\"hm\"]\n";
        std::fs::write(dir.path().join("codex.toml"), own).unwrap();
        let new = "id = \"zed\"\nname = \"Zed Agent\"\naliases = [\"zed-agent\"]\n[[rules]]\nid = \"busy\"\nlooks = \"working\"\ncontains = [\"busy\"]\n";
        std::fs::write(dir.path().join("zed.toml"), new).unwrap();
        // Not a rules file.
        std::fs::write(dir.path().join("notes.md"), "# notes").unwrap();

        let registry = Registry::load(Some(dir.path()));
        assert!(
            registry.all_problems().is_empty(),
            "{:?}",
            registry.all_problems()
        );
        let codex = registry.find("codex").unwrap();
        assert_eq!(
            codex.source,
            Source::Override(dir.path().join("codex.toml"))
        );
        assert_eq!(on(codex, "hm"), Some(Looks::Waiting));
        let zed = registry.find("zed-agent").unwrap();
        assert_eq!(zed.source, Source::Added(dir.path().join("zed.toml")));
        assert_eq!(zed.name, "Zed Agent");
        assert_eq!(on(zed, "busy"), Some(Looks::Working));
        // The others are crystal's still.
        assert_eq!(registry.find("claude").unwrap().source, Source::Bundled);
    }

    #[test]
    fn a_broken_file_leaves_crystal_s_rules_in_place_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex.toml"), "id = \"codex\"\n[[rules]\n").unwrap();
        std::fs::write(dir.path().join("mystery.toml"), "not toml at all [").unwrap();
        let registry = Registry::load(Some(dir.path()));
        let codex = registry.find("codex").unwrap();
        assert_eq!(codex.source, Source::Bundled);
        let problem = codex.problem.as_deref().unwrap();
        assert!(problem.starts_with("couldn't use "), "{problem}");
        assert!(
            problem.ends_with("its rules before it are used"),
            "{problem}"
        );
        assert_eq!(registry.problems.len(), 1);
        assert!(registry.problems[0].contains("mystery.toml"));

        // Two files for one agent: the first by name has it.
        let dir = tempfile::tempdir().unwrap();
        let rules = |look: &str| {
            format!(
                "id = \"claude\"\n[[rules]]\nid = \"r\"\nlooks = \"{look}\"\ncontains = [\"x\"]\n"
            )
        };
        std::fs::write(dir.path().join("a.toml"), rules("working")).unwrap();
        std::fs::write(dir.path().join("b.toml"), rules("waiting")).unwrap();
        let registry = Registry::load(Some(dir.path()));
        assert_eq!(
            on(registry.find("claude").unwrap(), "x"),
            Some(Looks::Working)
        );
        assert!(registry.problems[0].contains("b.toml has rules for claude"));
    }

    #[test]
    fn explain_says_which_rule_decided_and_why_the_others_didnt() {
        let rows = rows("• Working (5s • esc to interrupt)\n› fix it");
        let screen = Input {
            rows: &rows,
            title: "codex",
            progress: "",
        };
        let explained = bundled_registry().find("codex").unwrap().explain(&screen);
        assert_eq!(explained.looks, Meaning::Working);
        assert_eq!(
            explained.decided_by.as_deref(),
            Some("screen_working_fallback")
        );
        assert_eq!(explained.source, "bundled");
        // Tried highest first.
        assert_eq!(explained.rules[0].id, "osc_title_blocked");
        assert_eq!(
            explained.rules[0].why_not.as_deref(),
            Some("no \"action required\"")
        );
        let idle = explained
            .rules
            .iter()
            .find(|r| r.id == "osc_title_idle")
            .unwrap();
        assert!(idle.matched, "it matches, but it's outranked");
        assert_eq!(idle.seen, "codex");
        let any = explained
            .rules
            .iter()
            .find(|r| r.id == "live_strong_blocker")
            .unwrap();
        assert!(
            any.why_not
                .as_deref()
                .unwrap()
                .starts_with("none of any[] passes")
        );
    }

    #[test]
    fn each_agent_s_rules_read_its_screens() {
        let cases: &[(&str, &str, &str, &str, Option<Looks>)] = &[
            (
                "agy",
                "Requesting permission for:\n  rm -rf build\nDo you want to proceed?",
                "",
                "",
                Some(Looks::Waiting),
            ),
            ("agy", "⠋ Thinking about it", "", "", Some(Looks::Working)),
            ("amp", "> ", "⠋ fix the tests", "", Some(Looks::Working)),
            (
                "amp",
                "Run this command?\n  ls",
                "",
                "",
                Some(Looks::Waiting),
            ),
            (
                "amp",
                "> ",
                "project - amp - main",
                "",
                Some(Looks::Settled),
            ),
            (
                "cline",
                "Let Cline use this tool?\n  read_file",
                "",
                "",
                Some(Looks::Waiting),
            ),
            (
                "copilot",
                "Pick one\n  enter to select · esc to cancel",
                "",
                "",
                Some(Looks::Waiting),
            ),
            (
                "copilot",
                "  ◎ Waiting for background agents",
                "",
                "",
                Some(Looks::Working),
            ),
            (
                "cursor",
                "Run this command?\n  ls\nWaiting for approval...\n  Run (once) (y)",
                "",
                "",
                Some(Looks::Waiting),
            ),
            (
                "cursor",
                "  Generating\n  ctrl+c to stop",
                "",
                "",
                Some(Looks::Working),
            ),
            (
                "devin",
                "  running tools · esc to interrupt",
                "",
                "",
                Some(Looks::Working),
            ),
            (
                "droid",
                "⠋ Thinking... (esc to stop)",
                "",
                "",
                Some(Looks::Working),
            ),
            (
                "gemini",
                "│ Apply this change?\n│ ● Yes",
                "",
                "",
                Some(Looks::Waiting),
            ),
            (
                "gemini",
                "⠋ Thinking (esc to cancel, 3s)",
                "",
                "",
                Some(Looks::Working),
            ),
            ("grok", "> ", "Action Required", "", Some(Looks::Waiting)),
            ("hermes", "> ", "⏳ thinking", "", Some(Looks::Working)),
            ("hermes", "> ", "⚠ approve", "", Some(Looks::Waiting)),
            (
                "kilo",
                "△ Permission required\n  bash ls",
                "",
                "",
                Some(Looks::Waiting),
            ),
            (
                "kilo",
                "  working · esc interrupt",
                "",
                "",
                Some(Looks::Working),
            ),
            (
                "kimi",
                "Requesting approval\n  Approve once\n  Reject\n↵ confirm",
                "",
                "",
                Some(Looks::Waiting),
            ),
            ("kiro", "> ", "", "4;3", Some(Looks::Working)),
            (
                "kiro",
                "> Ask a question or describe a task",
                "",
                "4;3",
                Some(Looks::Settled),
            ),
            ("letta", "> ", "", "4;3", Some(Looks::Waiting)),
            (
                "maki",
                "Permission required\n  y allow  n deny",
                "",
                "",
                Some(Looks::Waiting),
            ),
            // MastraCode's screen says nothing: its hooks do.
            ("mastracode", "⠋ Working  esc to interrupt", "", "", None),
            ("mastracode", "", "", "", None),
            (
                "muse",
                "Do you trust this workspace?\n  Trust and continue",
                "",
                "",
                Some(Looks::Waiting),
            ),
            (
                "opencode",
                "△ Permission required\n  bash ls",
                "",
                "",
                Some(Looks::Waiting),
            ),
            (
                "opencode",
                "  esc to interrupt",
                "",
                "",
                Some(Looks::Working),
            ),
            ("opencode", "  ■■■■⬝⬝⬝⬝", "", "", Some(Looks::Working)),
            ("pi", "Working...", "", "", Some(Looks::Working)),
            ("pi", "> ", "", "", Some(Looks::Settled)),
            (
                "qodercli",
                "⠋ Thinking (esc to cancel, 3s)",
                "",
                "",
                Some(Looks::Working),
            ),
            (
                "qodercli",
                "Allow once or always?",
                "",
                "",
                Some(Looks::Waiting),
            ),
            ("qwen", "> ", "◐ Fixing", "", Some(Looks::Working)),
            ("qwen", "> ", "✳ Waiting", "", Some(Looks::Waiting)),
            (
                "qwen",
                "Do you trust this folder?\n  Trust folder (enter)\n  Don't trust (esc)",
                "",
                "",
                Some(Looks::Waiting),
            ),
        ];
        for (agent, screen, title, progress, expected) in cases {
            assert_eq!(
                read(agent, screen, title, progress),
                *expected,
                "{agent} on {screen:?}, titled {title:?}"
            );
        }
        // Every agent crystal has rules for is in there.
        for agent in bundled_registry().agents() {
            let tried = cases.iter().any(|case| case.0 == agent.id)
                || ["claude", "codex", DEFAULT].contains(&agent.id.as_str());
            assert!(tried, "no screen read for {}", agent.id);
        }
    }
}
