//! Flows: a named chain of steps, run one after another on one goal. Each
//! step is a background task (`claude -p`) with a profile of its own, asked
//! what its prompt says, which can bring in the goal, what the step before
//! it answered, and notes from the last time the flow was sent back. A
//! step can stop the flow at a gate until the user says to go on, or sends
//! it back.
//!
//! Flows are written in the config file as `[[flow]]` tables, each step a
//! `[[flow.step]]` under it. Running one is [`crate::flow_run`]'s job, and
//! the daemon's.
//!
//! Everything flows add to crystal goes through [`enabled`], so they can be
//! switched off as one.

use crate::config::Config;
use crate::profile::Profile;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// Whether flows are on. They always are for now; this is the one place
/// that will say otherwise once they can be switched off.
pub fn enabled(_config: &Config) -> bool {
    true
}

/// What a command about flows says while they're off.
pub const DISABLED: &str = "the flows plugin is off";

/// Refuses a command that's only about flows while they're off.
pub fn ensure_enabled(config: &Config) -> Result<()> {
    if !enabled(config) {
        bail!(DISABLED);
    }
    Ok(())
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

/// One step of a flow: a background task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// What the step is called, in its flow and in its session's name.
    pub name: String,
    /// The `[[profile]]` it runs with, for its model, mode, arguments,
    /// instructions and prompt: a Claude Code one, since only Claude Code
    /// runs as a background task. Left out, Claude Code as it's set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// What it's asked, with `{goal}`, `{previous}` and `{feedback}` filled
    /// in: see [`fill`].
    pub prompt: String,
    /// Run in a worktree the flow makes for itself, on a branch named after
    /// the goal. The steps after it run there too.
    #[serde(default, skip_serializing_if = "is_false")]
    pub worktree: bool,
    /// Stop after this step until the user goes on, or sends it back.
    #[serde(default, skip_serializing_if = "is_false")]
    pub gate: bool,
    /// The step that sending the flow back from this one's gate runs
    /// again, like `implement` for a review. Left out, this step itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub back_to: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Flow {
    /// A flow that can't run as written is an error that says why: no
    /// steps, a name with spaces, two steps with one name, a profile that
    /// isn't there or isn't Claude Code's, or a `back_to` that goes nowhere.
    pub fn check(&self, profiles: &[Profile]) -> Result<()> {
        let flow = &self.name;
        check_name("a flow", flow)?;
        if self.steps.is_empty() {
            bail!("flow {flow} has no steps: add [[flow.step]] tables under it");
        }
        for (index, step) in self.steps.iter().enumerate() {
            let name = &step.name;
            check_name(&format!("a step of flow {flow}"), name)?;
            if self.steps[..index]
                .iter()
                .any(|before| before.name == *name)
            {
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
                if profile.agent != "claude" {
                    bail!(
                        "step {name} of flow {flow} runs with profile {wanted}, which is for {}: \
                         steps run as background tasks, which only Claude Code can be",
                        profile.agent
                    );
                }
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
        }
        Ok(())
    }

    /// Where its steps' names are in order: `plan → implement → review`.
    pub fn chain(&self) -> String {
        let names: Vec<&str> = self.steps.iter().map(|step| step.name.as_str()).collect();
        names.join(" → ")
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

/// `template` with each `{name}` that `values` has filled in, in one pass,
/// so a value that happens to hold `{feedback}` isn't filled in again. A
/// `{` that starts no name it knows stays as it is, so a prompt can show
/// code.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut filled = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        filled.push_str(&rest[..open]);
        let from_brace = &rest[open..];
        let known = values.iter().find_map(|(name, value)| {
            let after = from_brace.strip_prefix(&format!("{{{name}}}"))?;
            Some((value, after))
        });
        match known {
            Some((value, after)) => {
                filled.push_str(value);
                rest = after;
            }
            None => {
                filled.push('{');
                rest = &from_brace[1..];
            }
        }
    }
    filled.push_str(rest);
    filled
}

/// What `crystal flow example` prints: a flow from plan to pull request,
/// with the profiles it runs with, to copy into the config file.
pub const EXAMPLE: &str = r#"# A flow from plan to pull request. Copy it into your config file
# (`crystal config` says where it is), then run it:
#
#     crystal flow run ship "Retry the webhook when it times out"
#
# Each step is a background task: Claude Code without a terminal, with the
# profile the step names. Nobody is there to say yes to a permission, so
# each profile says what its step may do.

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

# {goal} is what the flow was asked to do.
[[flow.step]]
name = "plan"
profile = "planner"
prompt = """
Plan how to do this: {goal}

Read the code it touches first. Answer with the plan: the files to change
and how, in the order to change them."""

# {previous} is what the step before answered. worktree = true makes a
# worktree for the flow, on a branch named after the goal; the steps after
# this one run there too.
[[flow.step]]
name = "implement"
profile = "builder"
worktree = true
prompt = """
Do this: {goal}

Follow this plan:
{previous}

{feedback}

Run the tests, and commit your work when they pass."""

# gate = true stops the flow here until you go on, or send it back with
# notes, which run the step back_to names again with {feedback} filled in:
#
#     crystal flow approve ship-1
#     crystal flow back ship-1 "Keep the old timeout as the default"
[[flow.step]]
name = "review"
profile = "reviewer"
gate = true
back_to = "implement"
prompt = """
Review the changes on this branch against what was asked: {goal}

What the builder said it did:
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
            worktree: false,
            gate: false,
            back_to: None,
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
        review.profile = Some("reviewer".into());
        let flow = flow(vec![step("plan"), review]);
        flow.check(&[profile("reviewer", "claude")]).unwrap();
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
    }

    #[test]
    fn a_step_runs_with_a_claude_code_profile_that_is_there() {
        let mut planned = step("plan");
        planned.profile = Some("planner".into());
        let flow = flow(vec![planned]);
        assert!(problem(&flow, &[]).contains("profile planner, which isn't there"));
        let codex = [profile("planner", "codex")];
        assert!(problem(&flow, &codex).contains("only Claude Code can be"));
    }

    #[test]
    fn going_back_takes_a_gate_and_a_step_at_or_before_it() {
        let mut ungated = step("review");
        ungated.back_to = Some("review".into());
        assert!(problem(&flow(vec![ungated]), &[]).contains("add `gate = true`"));

        let mut forward = step("plan");
        forward.gate = true;
        forward.back_to = Some("review".into());
        let flow = flow(vec![forward, step("review")]);
        assert!(problem(&flow, &[]).contains("isn't a step at or before it"));
    }

    #[test]
    fn a_template_has_what_it_names_filled_in() {
        let values = [("goal", "add retries"), ("previous", "the plan")];
        assert_eq!(
            fill("Do {goal}, following {previous}.", &values),
            "Do add retries, following the plan."
        );
        // A name it doesn't know, and a brace that starts none, stay.
        assert_eq!(fill("fn x() { {name} }", &values), "fn x() { {name} }");
    }

    #[test]
    fn a_value_is_never_filled_in_again() {
        let values = [("goal", "say {previous}"), ("previous", "the plan")];
        assert_eq!(fill("{goal}", &values), "say {previous}");
    }

    #[test]
    fn the_example_is_a_config_file_that_checks_out() {
        let config = crate::config::from_text(EXAMPLE).unwrap();
        let ship = &config.flows[0];
        assert_eq!(ship.chain(), "plan → implement → review → pr");
        assert!(ship.steps[1].worktree);
        assert_eq!(ship.steps[2].back_to.as_deref(), Some("implement"));
    }
}
