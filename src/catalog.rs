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
        settings: &[],
    },
    Agent {
        program: "opencode",
        name: "OpenCode",
        // Its bare argument is the project's directory.
        first_prompt: FirstPrompt::Option("--prompt"),
        settings: &[],
    },
    Agent {
        program: "cursor-agent",
        name: "Cursor",
        first_prompt: FirstPrompt::Argument,
        settings: &[],
    },
    Agent {
        program: "aider",
        name: "Aider",
        // `--message` runs one message and exits, which isn't a session.
        first_prompt: FirstPrompt::None,
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
