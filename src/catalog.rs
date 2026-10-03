//! The agents crystal knows how to start from the TUI's new-session panel:
//! what each is called, how it takes the task it's given, and the choices
//! the panel offers for it.
//!
//! Any program at all can run in a session; this list only decides what the
//! panel shows. An agent shows there when its program is on the PATH.

use std::path::Path;

/// An agent crystal knows.
#[derive(Debug, PartialEq, Eq)]
pub struct Agent {
    /// The program, as it's run: `claude`.
    pub program: &'static str,
    /// What people call it: `Claude Code`.
    pub name: &'static str,
    pub first_prompt: FirstPrompt,
    pub instructions: Instructions,
    /// The choices the panel offers for it, each a row of its own.
    pub settings: &'static [Setting],
}

/// How an agent is given its first prompt on its command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstPrompt {
    /// As its last argument: `claude "fix the tests"`.
    Argument,
    /// After this option: `gemini -i "fix the tests"`.
    Option(&'static str),
    /// It can't be: the task is typed once the agent is open.
    None,
}

impl FirstPrompt {
    /// Puts `task` on `command`, the way the agent takes a first prompt.
    pub fn add(self, command: &mut Vec<String>, task: &str) {
        if task.is_empty() {
            return;
        }
        match self {
            FirstPrompt::Argument => command.push(task.to_string()),
            FirstPrompt::Option(option) => {
                command.push(option.to_string());
                command.push(task.to_string());
            }
            FirstPrompt::None => {}
        }
    }
}

/// How an agent is given standing instructions for a whole session, on top
/// of its own: what a profile's `instructions` become on its command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Instructions {
    /// After this option: `claude --append-system-prompt "…"`.
    Option(&'static str),
    /// As this setting, given for the one run with `-c`, its value written
    /// as TOML: `codex -c developer_instructions="…"`.
    Setting(&'static str),
    /// It has no way to be given them.
    None,
}

impl Instructions {
    /// The arguments that give the agent `text` as its instructions.
    pub fn args(self, text: &str) -> Vec<String> {
        match self {
            Instructions::Option(option) => vec![option.to_string(), text.to_string()],
            Instructions::Setting(key) => {
                let quoted = toml_edit::Value::from(text).to_string();
                vec!["-c".to_string(), format!("{key}={quoted}")]
            }
            Instructions::None => Vec::new(),
        }
    }
}

/// A row of choices the panel offers for an agent: a label, the option
/// it sets, and what it can be set to. The first choice, "default", leaves
/// the option off, so the agent's own setting stands.
#[derive(Debug, PartialEq, Eq)]
pub struct Setting {
    pub label: &'static str,
    pub option: &'static str,
    pub choices: Choices,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Choices {
    /// These, as `(what the panel shows, what the option is given)`.
    Fixed(&'static [(&'static str, &'static str)]),
    /// Codex's models, which change as OpenAI ships them, so they're asked
    /// of Codex itself: see [`codex_models`].
    CodexModels,
}

pub const AGENTS: &[Agent] = &[
    Agent {
        program: "claude",
        name: "Claude Code",
        first_prompt: FirstPrompt::Argument,
        instructions: Instructions::Option("--append-system-prompt"),
        settings: &[
            Setting {
                label: "model",
                option: "--model",
                choices: Choices::Fixed(&[
                    ("default", ""),
                    ("opus", "opus"),
                    ("sonnet", "sonnet"),
                    ("haiku", "haiku"),
                ]),
            },
            Setting {
                label: "permissions",
                option: "--permission-mode",
                choices: Choices::Fixed(&[
                    ("ask", ""),
                    ("accept edits", "acceptEdits"),
                    ("plan", "plan"),
                    ("bypass", "bypassPermissions"),
                ]),
            },
        ],
    },
    Agent {
        program: "codex",
        name: "Codex",
        first_prompt: FirstPrompt::Argument,
        // Codex adds these to the session as a developer message.
        instructions: Instructions::Setting("developer_instructions"),
        settings: &[
            Setting {
                label: "model",
                option: "-m",
                choices: Choices::CodexModels,
            },
            Setting {
                label: "approvals",
                option: "-a",
                choices: Choices::Fixed(&[
                    ("default", ""),
                    ("on request", "on-request"),
                    ("never", "never"),
                ]),
            },
        ],
    },
    Agent {
        program: "gemini",
        name: "Gemini CLI",
        // Its bare argument has meant both interactive and one-shot over
        // its versions; `-i` has always meant "run this, then stay".
        first_prompt: FirstPrompt::Option("-i"),
        instructions: Instructions::None,
        settings: &[],
    },
    Agent {
        program: "opencode",
        name: "OpenCode",
        // Its bare argument is the project's directory.
        first_prompt: FirstPrompt::Option("--prompt"),
        instructions: Instructions::None,
        settings: &[],
    },
    Agent {
        program: "cursor-agent",
        name: "Cursor",
        first_prompt: FirstPrompt::Argument,
        instructions: Instructions::None,
        settings: &[],
    },
    Agent {
        program: "aider",
        name: "Aider",
        // `--message` runs one message and exits, which isn't a session.
        first_prompt: FirstPrompt::None,
        instructions: Instructions::None,
        settings: &[],
    },
];

impl Agent {
    /// The row that chooses its model, if it takes one.
    pub fn model_setting(&self) -> Option<&Setting> {
        self.settings
            .iter()
            .find(|setting| setting.label == "model")
    }

    /// The row that chooses how it asks before acting, if it has one.
    pub fn mode_setting(&self) -> Option<&Setting> {
        self.settings
            .iter()
            .find(|setting| setting.label != "model")
    }

    /// What its mode option can be given, past the default.
    pub fn mode_values(&self) -> Vec<&'static str> {
        match self.mode_setting().map(|setting| &setting.choices) {
            Some(Choices::Fixed(choices)) => choices
                .iter()
                .map(|(_, value)| *value)
                .filter(|value| !value.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// The agent run as `program`, if crystal knows it.
pub fn find(program: &str) -> Option<&'static Agent> {
    AGENTS.iter().find(|agent| agent.program == program)
}

/// The agent `command` runs, if crystal knows it, by its program's name.
fn agent_of(command: &[String]) -> Option<&'static Agent> {
    let program = command.first()?;
    find(Path::new(program).file_name()?.to_str()?)
}

/// Puts `task` on `command` as its agent's first prompt, the way that agent
/// takes one. A program crystal doesn't know is left as it is: there's no
/// telling where it would want a prompt, if anywhere.
pub fn add_first_prompt(command: &mut Vec<String>, task: &str) {
    let Some(agent) = agent_of(command) else {
        return;
    };
    match agent.first_prompt {
        FirstPrompt::Argument => command.push(task.to_string()),
        FirstPrompt::Option(option) => {
            command.push(option.to_string());
            command.push(task.to_string());
        }
        FirstPrompt::None => {}
    }
}

/// The first prompt `command` gives its agent, when that's plain to see:
/// an agent that takes it as an argument, given only that, as in `claude
/// "fix the tests"`, or given it after `--`. With options in between there's
/// no telling an option's value from a prompt, so it's left to the caller
/// to say.
pub fn first_prompt_in(command: &[String]) -> Option<String> {
    let agent = agent_of(command)?;
    if agent.first_prompt != FirstPrompt::Argument {
        return None;
    }
    let args = &command[1..];
    match args {
        [prompt] if !prompt.starts_with('-') => Some(prompt.clone()),
        _ => match args.iter().position(|arg| arg == "--") {
            Some(at) if at + 2 == args.len() => Some(args[at + 1].clone()),
            _ => None,
        },
    }
}

/// The agents whose programs are in one of the directories of `path`, a
/// PATH variable's value, in [`AGENTS`]' order.
pub fn installed_in(path: &str) -> Vec<&'static Agent> {
    AGENTS
        .iter()
        .filter(|agent| std::env::split_paths(path).any(|dir| is_program(&dir.join(agent.program))))
        .collect()
}

/// The agents installed on this machine, as this process's PATH finds them.
pub fn installed() -> Vec<&'static Agent> {
    let path = std::env::var("PATH").unwrap_or_default();
    installed_in(&path)
}

/// Whether `path` is a file that can be run.
fn is_program(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// The models Codex lists for choosing, from what `codex debug models`
/// prints: each model's slug, for those shown in Codex's own list.
pub fn codex_models(printed: &str) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Listing {
        models: Vec<Model>,
    }
    #[derive(serde::Deserialize)]
    struct Model {
        slug: String,
        #[serde(default)]
        visibility: String,
    }
    // Codex may print a note before its JSON.
    let Some(start) = printed.find('{') else {
        return Vec::new();
    };
    let Ok(listing) = serde_json::from_str::<Listing>(&printed[start..]) else {
        return Vec::new();
    };
    listing
        .models
        .into_iter()
        .filter(|model| model.visibility == "list")
        .map(|model| model.slug)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn a_task_goes_where_each_agent_takes_its_first_prompt() {
        let mut claude = words(&["claude", "--model", "opus"]);
        add_first_prompt(&mut claude, "fix it");
        assert_eq!(claude, ["claude", "--model", "opus", "fix it"]);

        let mut gemini = words(&["gemini"]);
        add_first_prompt(&mut gemini, "fix it");
        assert_eq!(gemini, ["gemini", "-i", "fix it"]);

        let mut aider = words(&["aider"]);
        add_first_prompt(&mut aider, "fix it");
        assert_eq!(aider, ["aider"], "aider takes no first prompt");

        let mut unknown = words(&["sleep", "30"]);
        add_first_prompt(&mut unknown, "fix it");
        assert_eq!(unknown, ["sleep", "30"], "a program crystal doesn't know");
    }

    #[test]
    fn a_first_prompt_is_found_only_where_it_is_plain_to_see() {
        let prompt = |args: &[&str]| first_prompt_in(&words(args));
        assert_eq!(
            prompt(&["claude", "fix the tests"]),
            Some("fix the tests".into())
        );
        assert_eq!(
            prompt(&["/usr/local/bin/codex", "--model", "o3", "--", "fix it"]),
            Some("fix it".into())
        );
        assert_eq!(prompt(&["claude", "--model", "opus"]), None);
        assert_eq!(prompt(&["claude", "--model", "opus", "fix it"]), None);
        assert_eq!(prompt(&["vim", "notes.md"]), None);
        assert_eq!(prompt(&["gemini", "-i", "fix it"]), None);
    }

    #[test]
    fn an_agent_shows_when_its_program_is_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let codex = dir.path().join("codex");
        std::fs::write(&codex, "#!/bin/sh\n").unwrap();
        let mut permissions = std::fs::metadata(&codex).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&codex, permissions).unwrap();
        // A file that can't be run isn't a program.
        std::fs::write(dir.path().join("claude"), "").unwrap();

        let path = format!("/nowhere:{}", dir.path().display());
        let found: Vec<&str> = installed_in(&path).iter().map(|a| a.program).collect();
        assert_eq!(found, ["codex"]);
    }

    #[test]
    fn each_agent_is_given_instructions_its_own_way() {
        let claude = find("claude").unwrap().instructions;
        assert_eq!(
            claude.args("Be brief."),
            ["--append-system-prompt", "Be brief."]
        );
        // Codex reads the value as TOML: it must come back as the text.
        let codex = find("codex").unwrap().instructions;
        let text = "Say \"done\" at the end,\nthen stop.";
        let args = codex.args(text);
        assert_eq!(args[0], "-c");
        let setting: toml::Table = toml::from_str(&args[1]).unwrap();
        assert_eq!(setting["developer_instructions"].as_str(), Some(text));
        assert!(find("aider").unwrap().instructions.args("x").is_empty());
    }

    #[test]
    fn codex_lists_only_the_models_it_shows() {
        let printed = r#"note
{"models":[{"slug":"gpt-6-luna","visibility":"list"},
           {"slug":"gpt-reserve","visibility":"hide"},
           {"slug":"gpt-5.5","visibility":"list"}]}"#;
        assert_eq!(codex_models(printed), ["gpt-6-luna", "gpt-5.5"]);
        assert!(codex_models("not json").is_empty());
    }

    #[test]
    fn each_known_agents_default_choice_leaves_its_option_off() {
        for agent in AGENTS {
            for setting in agent.settings {
                if let Choices::Fixed(choices) = setting.choices {
                    assert_eq!(choices[0].1, "", "{} {}", agent.program, setting.label);
                }
            }
        }
    }
}
