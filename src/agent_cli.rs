//! `crystal agent`: the agents crystal reads the screens of, where each
//! one's rules come from and whether it has hooks (`crystal integration`
//! puts them in); why it reads a session's agent the way it does; and the
//! rules it comes with, to start a file of one's own from.

use crate::agent_rules::{self, Explained, Input, Meaning};
use crate::client;
use crate::integration::{self, Agent};
use crate::protocol::{Front, Request, Response, ScreenExplained};
use crate::shell;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::path::Path;

/// Lists every agent crystal has rules for.
pub fn list(json: bool) -> Result<()> {
    let registry = agent_rules::current();
    let rows: Vec<Row> = registry
        .agents()
        .into_iter()
        .map(|rules| Row::of(rules))
        .collect();
    if json {
        let rows: Vec<_> = rows
            .iter()
            .map(|row| {
                json!({
                    "id": row.id,
                    "name": row.name,
                    "rules": row.rules,
                    "problem": row.problem,
                    "installed": row.installed,
                    "hooks": row.hooks,
                })
            })
            .collect();
        let problems = &registry.problems;
        let out = json!({ "agents": rows, "problems": problems });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    let table: Vec<[String; 5]> = rows
        .iter()
        .map(|row| {
            [
                row.id.clone(),
                row.name.clone(),
                row.rules.clone(),
                match row.installed {
                    Some(true) => "yes".into(),
                    Some(false) => "no".into(),
                    None => "-".into(),
                },
                row.hooks.unwrap_or("-").to_string(),
            ]
        })
        .collect();
    print_table(["AGENT", "NAME", "RULES", "INSTALLED", "HOOKS"], &table);
    let dir = shell::home_relative(&agent_rules::dir());
    println!();
    println!(
        "A file of your own in {dir}/ takes the place of crystal's rules for its agent, or adds an agent:"
    );
    println!("`crystal agent rules <agent>` prints crystal's to start from.");
    let problems: Vec<String> = rows
        .iter()
        .filter_map(|row| row.problem.clone())
        .chain(registry.problems.iter().cloned())
        .collect();
    if !problems.is_empty() {
        println!();
        for problem in problems {
            println!("! {problem}");
        }
    }
    Ok(())
}

/// One agent's line in the list.
struct Row {
    id: String,
    name: String,
    rules: String,
    problem: Option<String>,
    /// `None` for the rules for any agent, which isn't a program.
    installed: Option<bool>,
    hooks: Option<&'static str>,
}

impl Row {
    fn of(rules: &agent_rules::AgentRules) -> Row {
        let is_default = rules.id == agent_rules::DEFAULT;
        let names: Vec<&str> = std::iter::once(rules.id.as_str())
            .chain(rules.aliases.iter().map(String::as_str))
            .collect();
        // Claude Code gets crystal's hooks as crystal starts it; the others
        // crystal can hook take them from their own settings, from `crystal
        // integration`. Claude Code's there are for one typed into a shell.
        let crystal = std::env::current_exe().unwrap_or_default();
        let hooks = Agent::of_program(&rules.id).map(|agent| {
            let installed = integration::standing_of(agent, &crystal)
                .map_or("not installed", |standing| standing.word());
            match agent {
                Agent::Claude if installed == "not installed" => "built in",
                Agent::Claude => "built in, and installed",
                _ => installed,
            }
        });
        Row {
            id: rules.id.clone(),
            name: rules.name.clone(),
            rules: rules.source.describe(),
            problem: rules.problem.clone(),
            installed: (!is_default).then(|| names.iter().any(|name| on_path(name))),
            hooks,
        }
    }
}

/// Whether `program` is on this process's PATH.
fn on_path(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| {
        dir.join(program)
            .metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}

fn print_table<const N: usize>(header: [&str; N], rows: &[[String; N]]) {
    let mut widths = header.map(|title| title.chars().count());
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut line = String::new();
        for (at, (cell, width)) in cells.iter().zip(widths).enumerate() {
            if at + 1 == N {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:width$}  "));
            }
        }
        println!("{}", line.trim_end());
    };
    line(header.to_vec());
    for row in rows {
        line(row.iter().map(String::as_str).collect());
    }
}

/// Shows why crystal reads `session`'s agent the way it does, by the rules
/// of `agent` when it's given.
pub fn explain(
    socket: &Path,
    session: &str,
    agent: Option<&str>,
    verbose: bool,
    json: bool,
) -> Result<()> {
    let request = Request::ExplainAgent {
        name: session.to_string(),
        agent: agent.map(String::from),
    };
    let Some(response) = client::ask(socket, &request, false)? else {
        bail!("no daemon is running on {}", socket.display());
    };
    let Response::Explained(explained) = response else {
        bail!("the daemon didn't explain {session}");
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&explained)?);
    } else {
        print!("{}", session_text(&explained, verbose));
    }
    Ok(())
}

/// Shows how `agent`'s rules read a screen saved in the file at `path`, a
/// row a line.
pub fn explain_file(
    path: &Path,
    agent: &str,
    title: &str,
    progress: &str,
    verbose: bool,
    json: bool,
) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("couldn't read {}", path.display()))?;
    let rows: Vec<String> = text.lines().map(String::from).collect();
    let screen = Input {
        rows: &rows,
        title,
        progress,
    };
    let registry = agent_rules::current();
    let Some(rules) = registry
        .find(agent)
        .or_else(|| (agent == agent_rules::DEFAULT).then(|| registry.for_program(agent)))
    else {
        bail!(
            "crystal has no rules for {agent}: `crystal agent list` shows the agents it has them for"
        );
    };
    let explained = rules.explain(&screen);
    if json {
        println!("{}", serde_json::to_string_pretty(&explained)?);
    } else {
        print!("{}", rules_text(&explained, verbose));
    }
    Ok(())
}

/// Prints the rules crystal comes with for `agent`.
pub fn rules(agent: &str) -> Result<()> {
    let Some(text) = agent_rules::bundled(agent) else {
        bail!(
            "crystal comes with no rules for {agent}: `crystal agent list` shows the agents it has them for"
        );
    };
    print!("{text}");
    Ok(())
}

/// `crystal agent explain`'s text for a session.
fn session_text(explained: &ScreenExplained, verbose: bool) -> String {
    let mut out = String::new();
    let front = match &explained.front {
        Some(Front::Agent { program, name }) => format!("{name} ({program}) is in front"),
        Some(Front::Task) => "it's a background task".to_string(),
        Some(front) => format!("{} is in front", front.word()),
        None => "what's in front hasn't been looked at yet".to_string(),
    };
    out.push_str(&format!("{}: {front}\n", explained.session));
    if let Some(why) = &explained.not_read {
        out.push_str(&format!("its screen isn't read: {why}\n"));
    }
    let mut watch = format!("crystal has its screen as {}", explained.watch);
    if let Some(candidate) = explained.candidate {
        watch.push_str(&format!(
            "; {candidate} was seen once, and counts when it's seen again"
        ));
    }
    out.push_str(&format!("{watch}\n"));
    let status = explained
        .activity
        .map_or("nothing yet".to_string(), |activity| activity.to_string());
    out.push_str(&format!("its agent's status: {status}\n"));
    match &explained.rules {
        Some(rules) => {
            out.push('\n');
            out.push_str(&rules_text(rules, verbose));
        }
        None => out.push_str("no rules were tried: there's no agent to try them for\n"),
    }
    out
}

/// The rules tried on a screen, as text: what they read, from where, and
/// each rule in the order they're tried.
fn rules_text(explained: &Explained, verbose: bool) -> String {
    let mut out = String::new();
    let read = match explained.looks {
        Meaning::Skip => "nothing either way: the look stays as it was".to_string(),
        looks => looks.to_string(),
    };
    let by = match &explained.decided_by {
        Some(rule) => format!("by rule {rule}"),
        None => "as no rule matched".to_string(),
    };
    out.push_str(&format!(
        "{} ({}) rules read the screen now as {read}, {by}\n",
        explained.name, explained.agent
    ));
    let source = match explained.source.as_str() {
        "bundled" => "crystal's own".to_string(),
        path => path.to_string(),
    };
    out.push_str(&format!("rules: {source}\n"));
    if let Some(problem) = &explained.problem {
        out.push_str(&format!("! {problem}\n"));
    }
    out.push('\n');
    let mut widths = [4, 5, 8, 6];
    for rule in &explained.rules {
        widths[0] = widths[0].max(rule.id.chars().count());
        widths[1] = widths[1].max(rule.looks.to_string().len());
        widths[2] = widths[2].max(rule.priority.to_string().len());
        widths[3] = widths[3].max(rule.region.chars().count());
    }
    let [id, looks, priority, region] = widths;
    out.push_str(&format!(
        "  {:id$}  {:looks$}  {:>priority$}  {:region$}  RESULT\n",
        "RULE", "LOOKS", "PRIORITY", "REGION"
    ));
    for rule in &explained.rules {
        let decided = explained.decided_by.as_deref() == Some(rule.id.as_str()) && rule.matched;
        let mark = if decided {
            '→'
        } else if rule.matched {
            '✓'
        } else {
            ' '
        };
        let result = match &rule.why_not {
            None if decided => "matched, and decides".to_string(),
            None => "matched, outranked".to_string(),
            Some(why) => why.clone(),
        };
        out.push_str(&format!(
            "{mark} {:id$}  {:looks$}  {:>priority$}  {:region$}  {result}\n",
            rule.id,
            rule.looks.to_string(),
            rule.priority,
            rule.region
        ));
        if verbose {
            let seen = if rule.seen.is_empty() {
                "(empty)".to_string()
            } else {
                rule.seen.replace('\n', "\n      ")
            };
            out.push_str(&format!("      {}\n", seen.trim_end()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Activity;

    fn explained(rows: &[&str], title: &str) -> Explained {
        let rows: Vec<String> = rows.iter().map(|row| row.to_string()).collect();
        let screen = Input {
            rows: &rows,
            title,
            progress: "",
        };
        agent_rules::bundled_registry()
            .for_program("codex")
            .explain(&screen)
    }

    #[test]
    fn the_rule_that_decides_is_marked_and_the_rest_say_why_not() {
        let explained = explained(
            &["  Would you like to run the following command?", "› 1. Yes"],
            "",
        );
        let text = rules_text(&explained, false);
        assert!(
            text.starts_with("Codex (codex) rules read the screen now as waiting, by rule approval_question\nrules: crystal's own\n"),
            "{text}"
        );
        let decided = text.lines().find(|line| line.starts_with('→')).unwrap();
        assert!(decided.contains("approval_question"), "{decided}");
        assert!(decided.ends_with("matched, and decides"), "{decided}");
        let title = text
            .lines()
            .find(|line| line.contains("osc_title_blocked"))
            .unwrap();
        assert!(title.ends_with("no \"action required\""), "{title}");
    }

    #[test]
    fn verbose_shows_what_each_rule_looked_at() {
        let explained = explained(&["› fix it"], "codex");
        let text = rules_text(&explained, true);
        assert!(text.contains("\n      codex\n"), "{text}");
        assert!(text.contains("(empty)"), "{text}");
    }

    #[test]
    fn a_session_says_what_s_in_front_and_what_crystal_has_seen() {
        let session = ScreenExplained {
            session: "fix-it".into(),
            front: Some(Front::Shell { name: "zsh".into() }),
            not_read: Some("no agent is in front, but zsh".into()),
            watch: Meaning::Settled,
            candidate: Some(Meaning::Working),
            activity: Some(Activity::Idle),
            rules: None,
        };
        let text = session_text(&session, false);
        assert_eq!(
            text,
            "fix-it: zsh is in front\n\
             its screen isn't read: no agent is in front, but zsh\n\
             crystal has its screen as settled; working was seen once, and counts when it's seen again\n\
             its agent's status: idle\n\
             no rules were tried: there's no agent to try them for\n"
        );
    }
}
