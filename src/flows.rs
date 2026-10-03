//! Flows: a named chain of steps, run one after another on one goal. Each
//! step is a task with a profile of its own: Claude Code in the background
//! (`claude -p`), or any other agent in a terminal, which ends as its task
//! closes. A step is asked what its prompt says, which can bring in the
//! goal, what earlier steps answered and the files they kept, and notes
//! from the last time the flow was sent back. A step runs where the run
//! started, in a worktree the run makes for itself, or where the step
//! before it ran. It can stop the flow at a gate until the user says to go
//! on, or sends it back, a few rounds at most.
//!
//! Flows are written in the config file as `[[flow]]` tables, each step a
//! `[[flow.step]]` under it, and a project can keep its own in
//! [`REPO_FILE`], which take the place of the config's of the same name.
//! Running one is [`crate::flow_run`]'s job, and the daemon's.
//!
//! Everything flows add to crystal goes through [`enabled`], so they can be
//! switched off as one.

use crate::catalog::{self, FirstPrompt};
use crate::config::{self, Config};
use crate::plugins;
use crate::profile::Profile;
use crate::project;
use crate::shell;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The most steps a flow can have.
pub const MAX_STEPS: usize = 12;

/// The most rounds a gate can send a flow back for: the first, and two
/// more.
pub const MAX_ROUNDS: u32 = 3;

/// Where a project keeps flows of its own, in its main worktree.
pub const REPO_FILE: &str = ".crystal/flows.toml";

/// What an answer cut short to fit a prompt ends with.
pub const CUT: &str = "[cut short]";

/// The longest a goal's slug gets.
const SLUG_BYTES: usize = 40;

/// Whether flows are on: the `flows` plugin.
pub fn enabled(config: &Config) -> bool {
    plugins::enabled(config, "flows")
}

/// Refuses a command that's only about flows while they're off.
pub fn ensure_enabled(config: &Config) -> Result<()> {
    plugins::ensure_enabled(config, "flows")
}

/// A named chain of steps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    /// What it's run by: `crystal flow run <name>`. Its runs and their
    /// sessions are named after it, so it can't have spaces.
    pub name: String,
    /// A line on what it's for, shown in the new-session panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The steps, in the order they run: `[[flow.step]]` tables.
    #[serde(rename = "step", default)]
    pub steps: Vec<Step>,
}

/// One step of a flow: a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// What the step is called, in its flow and in its session's name.
    pub name: String,
    /// The `[[profile]]` it runs with, for its agent, model, mode,
    /// arguments, instructions and prompt. Claude Code's run in the
    /// background; any other agent runs in a terminal. Left out, Claude
    /// Code as it's set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// What it's asked, with `{goal}` and the rest filled in: see
    /// [`crate::flow_run::FlowRun::prompt_for`].
    pub prompt: String,
    /// Where it runs. Left out, where the step before it ran, or for the
    /// first step, where the run started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
    /// `placement = "fresh"`, as it was first written.
    #[serde(default, skip_serializing_if = "is_false")]
    pub worktree: bool,
    /// Stop after this step until the user goes on, or sends it back.
    #[serde(default, skip_serializing_if = "is_false")]
    pub gate: bool,
    /// The step that sending the flow back from this one's gate runs
    /// again, like `implement` for a review. Left out, this step itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_to: Option<String>,
    /// On a gate: how many rounds the flow may take through it, 1 to
    /// [`MAX_ROUNDS`]; in the last, it can't be sent back. Left out,
    /// [`MAX_ROUNDS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
}

/// Where a step runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    /// Where the run was started.
    Root,
    /// In the worktree the run makes for itself, on a branch named after
    /// the goal, the first time a step asks for it; every `fresh` step of
    /// the run after that runs there too.
    Fresh,
    /// Where the step before it ran.
    Same,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Step {
    /// How many rounds the flow may take through this step's gate.
    pub fn rounds(&self) -> u32 {
        self.max_rounds.unwrap_or(MAX_ROUNDS)
    }
}

impl Flow {
    /// A flow that can't run as written is an error that says why: no
    /// steps or too many, a name with spaces, two steps with one name, a
    /// profile that isn't there or whose agent can't be given a prompt, a
    /// placement or a `back_to` that goes nowhere, rounds out of reach, or
    /// a step that asks for a step that doesn't come before it.
    pub fn check(&self, profiles: &[Profile]) -> Result<()> {
        let flow = &self.name;
        check_name("a flow", flow)?;
        if self.steps.is_empty() {
            bail!("flow {flow} has no steps: add [[flow.step]] tables under it");
        }
        if self.steps.len() > MAX_STEPS {
            bail!(
                "flow {flow} has {} steps, and a flow can have {MAX_STEPS} at most",
                self.steps.len()
            );
        }
        for (index, step) in self.steps.iter().enumerate() {
            let name = &step.name;
            check_name(&format!("a step of flow {flow}"), name)?;
            let before = &self.steps[..index];
            if before.iter().any(|before| before.name == *name) {
                bail!("flow {flow} has two steps called {name}");
            }
            if step.prompt.trim().is_empty() {
                bail!("step {name} of flow {flow} has no prompt");
            }
            if let Some(wanted) = &step.profile {
                let Some(profile) = profiles.iter().find(|profile| profile.name == *wanted) else {
                    bail!(
                        "step {name} of flow {flow} runs with profile {wanted}, which isn't there"
                    );
                };
                let agent = catalog::find(&profile.agent);
                if agent.is_none_or(|agent| agent.first_prompt == FirstPrompt::None) {
                    bail!(
                        "step {name} of flow {flow} runs with profile {wanted}, for {}, which \
                         can't be given a prompt to start on",
                        profile.agent
                    );
                }
            }
            if step.worktree && step.placement.is_some() {
                bail!(
                    "step {name} of flow {flow} has both `worktree` and `placement`: \
                     `worktree = true` is `placement = \"fresh\"`, so keep one"
                );
            }
            if index == 0 && step.placement == Some(Placement::Same) {
                bail!(
                    "step {name} of flow {flow} is placed \"same\", but it's the first: there's \
                     no step before it to run where it ran"
                );
            }
            if let Some(back_to) = &step.back_to {
                if !step.gate {
                    bail!(
                        "step {name} of flow {flow} says where to go back to, but it has no gate \
                         to be sent back from: add `gate = true`"
                    );
                }
                if !self.steps[..=index]
                    .iter()
                    .any(|step| step.name == *back_to)
                {
                    bail!(
                        "step {name} of flow {flow} goes back to {back_to}, which isn't a step \
                         at or before it"
                    );
                }
            }
            if let Some(rounds) = step.max_rounds {
                if !step.gate {
                    bail!(
                        "step {name} of flow {flow} has max_rounds, but it has no gate to be \
                         sent back from: add `gate = true`"
                    );
                }
                if !(1..=MAX_ROUNDS).contains(&rounds) {
                    bail!(
                        "step {name} of flow {flow} has max_rounds = {rounds}: it can be 1 to \
                         {MAX_ROUNDS}"
                    );
                }
            }
            for named in steps_asked_for(&step.prompt) {
                if !before.iter().any(|before| before.name == named) {
                    bail!(
                        "step {name} of flow {flow} asks for {{{named}.…}}, but there's no step \
                         called {named} before it"
                    );
                }
            }
        }
        Ok(())
    }

    /// Where its steps' names are in order: `plan → implement → review`.
    pub fn chain(&self) -> String {
        let names: Vec<&str> = self.steps.iter().map(|step| step.name.as_str()).collect();
        names.join(" → ")
    }

    /// Where step `step` runs: where it says, or with `worktree = true`,
    /// fresh; saying neither, where the step before it ran, or for the
    /// first, where the run started.
    pub fn placement(&self, step: usize) -> Placement {
        let wanted = &self.steps[step];
        match (wanted.placement, wanted.worktree) {
            (Some(placement), _) => placement,
            (None, true) => Placement::Fresh,
            (None, false) if step == 0 => Placement::Root,
            (None, false) => Placement::Same,
        }
    }
}

/// A flow's or a step's name goes into session names, which can't be
/// empty or have spaces.
fn check_name(what: &str, name: &str) -> Result<()> {
    if name.is_empty() || name.contains(char::is_whitespace) {
        bail!("{what} is called {name:?}: names can't be empty or have spaces");
    }
    Ok(())
}

/// The steps a prompt asks for by name: each `<step>` of a
/// `{<step>.summary}` or `{<step>.artifacts}` in it.
fn steps_asked_for(prompt: &str) -> Vec<&str> {
    let mut named = Vec::new();
    let mut rest = prompt;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('}') else {
            break;
        };
        if let Some((step, "summary" | "artifacts")) = rest[..close].rsplit_once('.')
            && !step.is_empty()
            && !step.contains(|c: char| c == '{' || c.is_whitespace())
        {
            named.push(step);
        }
    }
    named
}

/// What a `{name}` in a prompt is filled in with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Value<'a> {
    pub text: &'a str,
    /// For what a step answered, how old it is: its step's place in the
    /// flow, or `usize::MAX` for what's newest. A prompt that's too long
    /// has these cut, the oldest first. `None` for what's never cut.
    pub age: Option<usize>,
}

impl Value<'_> {
    /// A value that's never cut.
    pub fn kept(text: &str) -> Value<'_> {
        Value { text, age: None }
    }

    /// What a step answered, `age` old.
    pub fn answer(text: &str, age: usize) -> Value<'_> {
        Value {
            text,
            age: Some(age),
        }
    }
}

/// A prompt with its `{name}`s filled in, in pieces, so that one too long
/// can be cut where it may be.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filled {
    pieces: Vec<(String, Option<usize>)>,
}

impl Filled {
    /// Adds `text` to the end, `age` old, as [`Value::age`] says.
    pub fn push(&mut self, text: &str, age: Option<usize>) {
        match self.pieces.last_mut() {
            Some((last, None)) if age.is_none() => last.push_str(text),
            _ => self.pieces.push((text.to_string(), age)),
        }
    }

    pub fn text(&self) -> String {
        self.pieces.iter().map(|(text, _)| text.as_str()).collect()
    }

    /// The prompt, trimmed, in `budget` bytes at most: what steps answered
    /// is cut to fit, the oldest first, each ending in [`CUT`], down to
    /// [`CUT`] alone; nothing else ever is. `None` when it doesn't fit even
    /// so.
    pub fn within(mut self, budget: usize) -> Option<String> {
        let mut oldest_first: Vec<(usize, usize)> = self
            .pieces
            .iter()
            .enumerate()
            .filter_map(|(index, (_, age))| age.map(|age| (age, index)))
            .collect();
        oldest_first.sort_unstable();
        for (_, index) in oldest_first {
            let length = self.text().trim().len();
            if length <= budget {
                break;
            }
            let piece = &mut self.pieces[index].0;
            // Already as short as it gets.
            if piece.len() <= CUT.len() {
                continue;
            }
            let mut end = piece.len().saturating_sub(length - budget + CUT.len() + 1);
            while !piece.is_char_boundary(end) {
                end -= 1;
            }
            let kept = piece[..end].trim_end();
            *piece = if kept.is_empty() {
                CUT.to_string()
            } else {
                format!("{kept} {CUT}")
            };
        }
        let text = self.text();
        let text = text.trim();
        (text.len() <= budget).then(|| text.to_string())
    }
}

/// `template` with each `{name}` that `values` has filled in, in one pass,
/// so a value that happens to hold `{feedback}` isn't filled in again. A
/// `{` that starts no name it knows stays as it is, so a prompt can show
/// code.
pub fn fill(template: &str, values: &[(&str, Value)]) -> Filled {
    let mut filled = Filled::default();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        filled.push(&rest[..open], None);
        let from_brace = &rest[open..];
        let known = values.iter().find_map(|(name, value)| {
            let after = from_brace.strip_prefix(&format!("{{{name}}}"))?;
            Some((value, after))
        });
        match known {
            Some((value, after)) => {
                filled.push(value.text, value.age);
                rest = after;
            }
            None => {
                filled.push("{", None);
                rest = &from_brace[1..];
            }
        }
    }
    filled.push(rest, None);
    filled
}

/// A goal as a branch's name and `{slug}`: the words of its first line in
/// lower case, joined by `-`, as many as fit in 40 bytes. Empty when it has
/// no letters or digits.
pub fn slug(goal: &str) -> String {
    let line = goal.lines().find(|line| !line.trim().is_empty());
    let words = line
        .unwrap_or("")
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase);
    let mut slug = String::new();
    for word in words {
        let joined = if slug.is_empty() {
            word
        } else {
            format!("{slug}-{word}")
        };
        if joined.len() > SLUG_BYTES {
            // A first word too long for it is cut instead.
            if slug.is_empty() {
                slug = joined[..SLUG_BYTES].to_string();
            }
            break;
        }
        slug = joined;
    }
    slug
}

/// A flow as found, and where: in the config file or a project's
/// [`REPO_FILE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub flow: Flow,
    /// The file it's written in.
    pub file: PathBuf,
    /// Whether that's the project's own file.
    pub in_repo: bool,
    /// Why it can't run, when it can't. The config file's always can: the
    /// file isn't read at all otherwise.
    pub problem: Option<String>,
}

/// The flows a run started in `dir` finds: the config file's, in their
/// order, with the flows of the project `dir` is in taking the place of
/// those of the same name, and after the rest. Then what's wrong with the
/// project's file, when it can't be read at all.
pub fn definitions(config: &Config, dir: &Path) -> (Vec<Found>, Option<String>) {
    let config_file = config::path();
    let mut found: Vec<Found> = config
        .flows
        .iter()
        .map(|flow| Found {
            flow: flow.clone(),
            file: config_file.clone(),
            in_repo: false,
            problem: None,
        })
        .collect();
    let file = project::of(dir).path.join(REPO_FILE);
    let (theirs, problem) = match read_repo_file(&file, &config.profiles) {
        Ok(theirs) => (theirs, None),
        Err(err) => (Vec::new(), Some(format!("{err:#}"))),
    };
    for flow in theirs {
        match found
            .iter_mut()
            .find(|known| known.flow.name == flow.flow.name)
        {
            Some(same_name) => *same_name = flow,
            None => found.push(flow),
        }
    }
    (found, problem)
}

/// The flow called `name` that a run started in `dir` finds, ready to
/// run; or why there's none.
pub fn find(config: &Config, dir: &Path, name: &str) -> Result<Flow> {
    let (found, problem) = definitions(config, dir);
    if let Some(wanted) = found.iter().find(|found| found.flow.name == name) {
        if let Some(why) = &wanted.problem {
            bail!(
                "flow {name}, in {}, can't run: {why}",
                shell::home_relative(&wanted.file)
            );
        }
        return Ok(wanted.flow.clone());
    }
    let known: Vec<&str> = found.iter().map(|found| found.flow.name.as_str()).collect();
    let mut why = if known.is_empty() {
        format!("there's no flow called {name}, nor any other: `crystal flow example` shows one")
    } else {
        format!(
            "there's no flow called {name}; there's {}",
            known.join(", ")
        )
    };
    if let Some(problem) = problem {
        why.push_str(&format!(
            ", and the project's own couldn't be read: {problem}"
        ));
    }
    bail!(why)
}

/// What a project's [`REPO_FILE`] holds: `[[flow]]` tables and nothing
/// else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RepoFile {
    #[serde(rename = "flow", default)]
    flows: Vec<Flow>,
}

/// The flows in the project's file at `file`, each checked against the
/// config file's `profiles`: none when there's no file.
fn read_repo_file(file: &Path, profiles: &[Profile]) -> Result<Vec<Found>> {
    let shown = shell::home_relative(file);
    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("couldn't read {shown}")),
    };
    let read: RepoFile = toml::from_str(&text).with_context(|| format!("in {shown}"))?;
    let mut found: Vec<Found> = Vec::new();
    for flow in read.flows {
        let twice = found.iter().any(|before| before.flow.name == flow.name);
        let problem = if twice {
            Some(format!("two flows are called {}", flow.name))
        } else {
            flow.check(profiles).err().map(|err| format!("{err:#}"))
        };
        found.push(Found {
            flow,
            file: file.to_path_buf(),
            in_repo: true,
            problem,
        });
    }
    Ok(found)
}

/// What `crystal flow example` prints: a flow from plan to pull request,
/// with the profiles it runs with, to copy into the config file.
pub const EXAMPLE: &str = r#"# A flow from plan to pull request. Copy it into your config file
# (`crystal config` says where it is), or into your project's
# .crystal/flows.toml without the profiles, then run it:
#
#     crystal flow run ship "Retry the webhook when it times out"
#
# Each step is a task with the profile it names. Claude Code's run in the
# background, where nobody is there to say yes to a permission, so each
# profile says what its step may do. Any other agent runs in a terminal.

[[profile]]
name = "planner"
description = "Reads the code and plans; changes nothing"
agent = "claude"
mode = "plan"

[[profile]]
name = "builder"
description = "Makes the change and runs the tests"
agent = "claude"
mode = "acceptEdits"
args = ["--allowedTools", "Bash"]

[[profile]]
name = "reviewer"
description = "Reads the branch's changes; changes nothing"
agent = "claude"
model = "opus"
mode = "plan"
instructions = "You are reviewing, not writing. Say what must change before this is merged, risks first."

[[profile]]
name = "shipper"
description = "Pushes the branch and opens a pull request"
agent = "claude"
args = ["--allowedTools", "Bash(git:*)", "Bash(gh:*)"]

[[flow]]
name = "ship"
description = "Plan, build in a worktree, review, open a pull request"

# {goal} is what the flow was asked to do. The first step runs where the
# run was started.
[[flow.step]]
name = "plan"
profile = "planner"
prompt = """
Plan how to do this: {goal}

Read the code it touches first. Answer with the plan: the files to change
and how, in the order to change them."""

# {plan.summary} is what the plan step answered; {previous} is what the
# step just before answered. placement = "fresh" runs this step in a
# worktree the run makes for itself, on a branch named after the goal
# ({slug}).
[[flow.step]]
name = "implement"
profile = "builder"
placement = "fresh"
prompt = """
Do this: {goal}

Follow this plan:
{plan.summary}

{feedback}

Run the tests, and commit your work when they pass."""

# placement = "same" runs it where the step before it ran, as a step that
# says no placement does. gate = true stops the flow here until you go on,
# or send it back with notes, which runs the step back_to names again with
# {feedback} filled in, for up to max_rounds rounds:
#
#     crystal flow approve ship-1
#     crystal flow back ship-1 "Keep the old timeout as the default"
[[flow.step]]
name = "review"
profile = "reviewer"
placement = "same"
gate = true
back_to = "implement"
max_rounds = 3
prompt = """
Review the changes on this branch against what was asked: {goal}

What the builder said it did, in round {round}:
{previous}

{feedback}"""

[[flow.step]]
name = "pr"
profile = "shipper"
prompt = """
Push this branch and open a pull request for it with `gh pr create --fill`.
It does this: {goal}
Answer with the pull request's address."""
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str) -> Step {
        Step {
            name: name.into(),
            profile: None,
            prompt: "do {goal}".into(),
            placement: None,
            worktree: false,
            gate: false,
            back_to: None,
            max_rounds: None,
        }
    }

    fn flow(steps: Vec<Step>) -> Flow {
        Flow {
            name: "ship".into(),
            description: None,
            steps,
        }
    }

    fn profile(name: &str, agent: &str) -> Profile {
        Profile {
            name: name.into(),
            ..Profile::for_agent(agent)
        }
    }

    fn problem(flow: &Flow, profiles: &[Profile]) -> String {
        format!("{:#}", flow.check(profiles).unwrap_err())
    }

    #[test]
    fn a_flow_with_steps_that_can_run_checks_out() {
        let mut review = step("review");
        review.gate = true;
        review.back_to = Some("plan".into());
        review.max_rounds = Some(2);
        review.profile = Some("reviewer".into());
        review.prompt = "Review {plan.summary} and {plan.artifacts}".into();
        let flow = flow(vec![step("plan"), review]);
        flow.check(&[profile("reviewer", "codex")]).unwrap();
        assert_eq!(flow.chain(), "plan → review");
    }

    #[test]
    fn a_flow_that_cant_run_says_why() {
        assert!(problem(&flow(vec![]), &[]).contains("has no steps"));

        let mut spaced = flow(vec![step("plan")]);
        spaced.name = "ship it".into();
        assert!(problem(&spaced, &[]).contains("can't be empty or have spaces"));

        let twice = flow(vec![step("plan"), step("plan")]);
        assert!(problem(&twice, &[]).contains("two steps called plan"));

        let mut blank = step("plan");
        blank.prompt = "  ".into();
        assert!(problem(&flow(vec![blank]), &[]).contains("has no prompt"));

        let many = flow((0..=MAX_STEPS).map(|n| step(&format!("s{n}"))).collect());
        assert!(problem(&many, &[]).contains("13 steps, and a flow can have 12 at most"));
    }

    #[test]
    fn a_step_runs_with_a_profile_that_is_there_and_takes_a_prompt() {
        let mut planned = step("plan");
        planned.profile = Some("planner".into());
        let flow = flow(vec![planned]);
        assert!(problem(&flow, &[]).contains("profile planner, which isn't there"));
        let aider = [profile("planner", "aider")];
        assert!(problem(&flow, &aider).contains("can't be given a prompt to start on"));
        flow.check(&[profile("planner", "gemini")]).unwrap();
    }

    #[test]
    fn going_back_takes_a_gate_a_step_at_or_before_it_and_rounds_in_reach() {
        let mut ungated = step("review");
        ungated.back_to = Some("review".into());
        assert!(problem(&flow(vec![ungated]), &[]).contains("add `gate = true`"));

        let mut forward = step("plan");
        forward.gate = true;
        forward.back_to = Some("review".into());
        let ahead = flow(vec![forward, step("review")]);
        assert!(problem(&ahead, &[]).contains("isn't a step at or before it"));

        let mut rounds = step("review");
        rounds.max_rounds = Some(2);
        assert!(
            problem(&flow(vec![rounds.clone()]), &[]).contains("max_rounds, but it has no gate")
        );
        rounds.gate = true;
        rounds.max_rounds = Some(4);
        assert!(problem(&flow(vec![rounds]), &[]).contains("it can be 1 to 3"));
    }

    #[test]
    fn a_step_is_placed_where_it_says_or_where_the_one_before_ran() {
        let mut fresh = step("build");
        fresh.worktree = true;
        let mut root = step("check");
        root.placement = Some(Placement::Root);
        let flow = flow(vec![step("plan"), fresh, step("review"), root]);
        flow.check(&[]).unwrap();
        let placed: Vec<Placement> = (0..4).map(|step| flow.placement(step)).collect();
        use Placement::*;
        assert_eq!(placed, [Root, Fresh, Same, Root]);

        let mut first = step("plan");
        first.placement = Some(Same);
        assert!(problem(&self::flow(vec![first]), &[]).contains("it's the first"));
        let mut both = step("plan");
        both.worktree = true;
        both.placement = Some(Fresh);
        assert!(problem(&self::flow(vec![both]), &[]).contains("so keep one"));
    }

    #[test]
    fn a_step_asks_only_for_steps_before_it() {
        let mut ahead = step("plan");
        ahead.prompt = "Use {review.summary}".into();
        let flow = flow(vec![ahead, step("review")]);
        assert!(problem(&flow, &[]).contains("asks for {review.…}, but there's no step"));
        // Braces that ask for no step are left to the prompt.
        assert_eq!(
            steps_asked_for("fn x() { y.summary } {a b.summary}"),
            Vec::<&str>::new()
        );
        assert_eq!(
            steps_asked_for("{plan.summary} {build.artifacts} {x.y}"),
            ["plan", "build"]
        );
    }

    #[test]
    fn a_template_has_what_it_names_filled_in() {
        let values = [
            ("goal", Value::kept("add retries")),
            ("previous", Value::answer("the plan", 0)),
        ];
        assert_eq!(
            fill("Do {goal}, following {previous}.", &values).text(),
            "Do add retries, following the plan."
        );
        // A name it doesn't know, and a brace that starts none, stay.
        assert_eq!(
            fill("fn x() { {name} }", &values).text(),
            "fn x() { {name} }"
        );
    }

    #[test]
    fn a_value_is_never_filled_in_again() {
        let values = [
            ("goal", Value::kept("say {previous}")),
            ("previous", Value::kept("the plan")),
        ];
        assert_eq!(fill("{goal}", &values).text(), "say {previous}");
    }

    #[test]
    fn a_prompt_too_long_has_the_oldest_answers_cut_and_nothing_else() {
        let old = "o".repeat(100);
        let new = "n".repeat(100);
        let values = [
            ("goal", Value::kept("GOAL")),
            ("old", Value::answer(&old, 0)),
            ("new", Value::answer(&new, 1)),
        ];
        let filled = fill("{goal}: {old} then {new}", &values);
        assert_eq!(filled.clone().within(1000), Some(filled.text()));

        let cut = filled.clone().within(160).unwrap();
        assert!(cut.len() <= 160, "{}", cut.len());
        assert!(cut.starts_with("GOAL: ooo") && cut.contains(CUT), "{cut}");
        assert!(cut.ends_with(&new), "the newest stays whole: {cut}");

        let both = filled.clone().within(40).unwrap();
        assert!(both.starts_with(&format!("GOAL: {CUT} then ")), "{both}");
        assert!(both.len() <= 40, "{both}");
        // What can't be cut doesn't fit in less.
        assert_eq!(filled.within(10), None);
    }

    #[test]
    fn a_slug_is_the_goals_first_line_made_fit_for_a_branch() {
        assert_eq!(
            slug("Fix issue #42: login redirect"),
            "fix-issue-42-login-redirect"
        );
        assert_eq!(
            slug("make the export of the whole ledger stream instead of buffering"),
            "make-the-export-of-the-whole-ledger"
        );
        assert_eq!(slug("\nAdd dark mode\nand more"), "add-dark-mode");
        assert_eq!(slug(&"x".repeat(50)), "x".repeat(40));
        assert_eq!(slug("  "), "");
    }

    #[test]
    fn a_projects_flows_take_the_place_of_the_configs_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::from_text(EXAMPLE).unwrap();
        let (found, problem) = definitions(&config, dir.path());
        assert_eq!(problem, None);
        assert_eq!(found.len(), 1);
        assert!(!found[0].in_repo);

        fs::create_dir(dir.path().join(".crystal")).unwrap();
        let file = dir.path().join(REPO_FILE);
        fs::write(
            &file,
            "[[flow]]\nname = \"ship\"\n[[flow.step]]\nname = \"all\"\nprompt = \"Do {goal}\"\n\n\
             [[flow]]\nname = \"lint\"\n[[flow.step]]\nname = \"fix\"\nprofile = \"nobody\"\n\
             prompt = \"Lint\"\n",
        )
        .unwrap();
        let (found, problem) = definitions(&config, dir.path());
        assert_eq!(problem, None);
        let names: Vec<(&str, bool)> = found
            .iter()
            .map(|found| (found.flow.name.as_str(), found.in_repo))
            .collect();
        assert_eq!(names, [("ship", true), ("lint", true)]);
        assert_eq!(find(&config, dir.path(), "ship").unwrap().chain(), "all");
        let broken = format!("{:#}", find(&config, dir.path(), "lint").unwrap_err());
        assert!(
            broken.contains("can't run: step fix of flow lint"),
            "{broken}"
        );
        let none = format!("{:#}", find(&config, dir.path(), "nope").unwrap_err());
        assert!(none.contains("there's ship, lint"), "{none}");

        fs::write(&file, "[[flow]]\nnam = 1\n").unwrap();
        let (found, problem) = definitions(&config, dir.path());
        assert_eq!(found.len(), 1);
        assert!(problem.unwrap().contains("flows.toml"));
    }

    #[test]
    fn the_example_is_a_config_file_that_checks_out() {
        let config = crate::config::from_text(EXAMPLE).unwrap();
        let ship = &config.flows[0];
        assert_eq!(ship.chain(), "plan → implement → review → pr");
        assert_eq!(ship.placement(1), Placement::Fresh);
        assert_eq!(ship.placement(3), Placement::Same);
        assert_eq!(ship.steps[2].back_to.as_deref(), Some("implement"));
    }
}
