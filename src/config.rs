//! The settings in `~/.config/crystal/config.toml`. The file is optional,
//! and so is every setting in it; a setting left out has its default.
//!
//! A key crystal doesn't know is an error, not something to skip: a
//! setting spelled wrong would otherwise do nothing, without a word.

use crate::catalog;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Tell the user when a session needs them: its agent is asking them
    /// something, or has finished a turn nobody was watching.
    pub notify: bool,
    /// A shell command to run in place of the desktop notification, say to
    /// send it to a phone. It finds what happened in its environment: see
    /// [`crate::notify`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify_command: Option<String>,
    /// The agent the new-session panel picks at first, until one has been
    /// started from it: an agent's program, like `codex`, maybe with
    /// arguments for it, like `codex --full-auto`.
    pub new_session: String,
    /// The TUI's colors.
    pub theme: ThemeName,
    /// Saved ways to start a session, offered first in the new-session
    /// panel: `[[preset]]` tables in the file.
    #[serde(rename = "preset", skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
}

/// A saved way to start a session: an agent, set up a certain way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    /// What the panel calls it.
    pub name: String,
    /// The agent's program, one crystal knows: `claude`, `codex`, ….
    pub agent: String,
    /// The model, for an agent that takes one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How it asks before acting: Claude Code's `--permission-mode`, or
    /// Codex's `-a`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// More arguments for the agent, as they'd be written after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Text put in front of the task: what this preset always asks for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// The TUI's colors to choose from. `dark` and `light` paint their own
/// background, so they look the same in any terminal; `terminal` paints
/// nothing and keeps to the terminal's own colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    Dark,
    Light,
    Terminal,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            notify: true,
            notify_command: None,
            new_session: "claude".to_string(),
            theme: ThemeName::Dark,
            presets: Vec::new(),
        }
    }
}

impl Config {
    /// The settings in the config file, or the defaults when there's no
    /// file. A file that can't be read, or doesn't make sense, is an error
    /// that names it.
    pub fn load() -> Result<Config> {
        let path = path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Config::default());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("couldn't read {}", path.display()));
            }
        };
        parse(&text).with_context(|| format!("in {}", path.display()))
    }

    /// The settings written as TOML, the way the file would hold them.
    pub fn to_toml(&self) -> String {
        // Our own plain struct always makes valid TOML.
        toml::to_string(self).expect("settings make TOML")
    }
}

/// `$XDG_CONFIG_HOME/crystal/config.toml`, or else
/// `~/.config/crystal/config.toml`.
pub fn path() -> PathBuf {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config")
        }
    };
    base.join("crystal").join("config.toml")
}

fn parse(text: &str) -> Result<Config> {
    let config: Config = toml::from_str(text)?;
    for preset in &config.presets {
        check_preset(preset)?;
    }
    let mut names: Vec<&str> = config.presets.iter().map(|p| p.name.as_str()).collect();
    names.sort_unstable();
    if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
        bail!("two presets are called {}", pair[0]);
    }
    Ok(config)
}

/// A preset that can't be started as written is an error that says why:
/// an agent crystal doesn't know, or a model or mode its agent doesn't take.
fn check_preset(preset: &Preset) -> Result<()> {
    let name = &preset.name;
    if name.trim().is_empty() {
        bail!("a preset has no name");
    }
    let Some(agent) = catalog::find(&preset.agent) else {
        let known: Vec<&str> = catalog::AGENTS.iter().map(|a| a.program).collect();
        bail!(
            "preset {name}: crystal doesn't know the agent {}; it knows {}",
            preset.agent,
            known.join(", ")
        );
    };
    if preset.model.is_some() && agent.model_setting().is_none() {
        bail!("preset {name}: {} doesn't take a model", agent.name);
    }
    if let Some(mode) = &preset.mode {
        let modes = agent.mode_values();
        if modes.is_empty() {
            bail!("preset {name}: {} doesn't take a mode", agent.name);
        }
        if !modes.contains(&mode.as_str()) {
            bail!(
                "preset {name}: {mode} isn't a mode of {}; it takes {}",
                agent.name,
                modes.join(", ")
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_is_all_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn a_setting_given_replaces_its_default_only() {
        let config = parse("new_session = \"codex\"\n").unwrap();
        assert_eq!(config.new_session, "codex");
        assert!(config.notify);
        assert_eq!(config.notify_command, None);
    }

    #[test]
    fn a_key_spelled_wrong_is_an_error_that_names_it() {
        let err = parse("notfy = false\n").unwrap_err();
        assert!(format!("{err:#}").contains("notfy"), "{err:#}");
    }

    #[test]
    fn a_setting_of_the_wrong_kind_is_an_error() {
        assert!(parse("notify = \"yes\"\n").is_err());
    }

    #[test]
    fn a_theme_is_chosen_by_name() {
        assert_eq!(
            parse("theme = \"light\"\n").unwrap().theme,
            ThemeName::Light
        );
        assert_eq!(Config::default().theme, ThemeName::Dark);
    }

    #[test]
    fn a_theme_crystal_doesnt_have_is_an_error_that_names_it() {
        let err = parse("theme = \"neon\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("neon"), "{err:#}");
    }

    #[test]
    fn presets_are_read_in_order() {
        let config = parse(
            r#"
[[preset]]
name = "review"
agent = "claude"
mode = "plan"
prompt = "Review the diff on this branch."

[[preset]]
name = "fast"
agent = "codex"
model = "gpt-6-luna"
args = ["--search"]
"#,
        )
        .unwrap();
        let names: Vec<&str> = config.presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["review", "fast"]);
        assert_eq!(config.presets[0].mode.as_deref(), Some("plan"));
        assert_eq!(config.presets[1].args, ["--search"]);
    }

    #[test]
    fn a_preset_that_cant_start_is_an_error_that_says_why() {
        let cases = [
            (
                "name = \"x\"\nagent = \"vim\"",
                "doesn't know the agent vim",
            ),
            (
                "name = \"x\"\nagent = \"aider\"\nmodel = \"o3\"",
                "doesn't take a model",
            ),
            (
                "name = \"x\"\nagent = \"claude\"\nmode = \"yolo\"",
                "isn't a mode of Claude Code",
            ),
            (
                "name = \"x\"\nagent = \"claude\"\nflavor = \"mint\"",
                "flavor",
            ),
            (
                "name = \"x\"\nagent = \"claude\"\n[[preset]]\nname = \"x\"\nagent = \"codex\"",
                "two presets are called x",
            ),
        ];
        for (preset, expected) in cases {
            let err = parse(&format!("[[preset]]\n{preset}\n")).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
    }

    #[test]
    fn the_settings_written_out_read_back_the_same() {
        let config = Config {
            notify: false,
            notify_command: Some("say \"$CRYSTAL_NOTICE\"".into()),
            new_session: "codex --model o3".into(),
            theme: ThemeName::Terminal,
            presets: vec![Preset {
                name: "review".into(),
                agent: "claude".into(),
                model: Some("opus".into()),
                mode: Some("plan".into()),
                args: vec!["--verbose".into()],
                prompt: Some("Review it.".into()),
            }],
        };
        assert_eq!(parse(&config.to_toml()).unwrap(), config);
        assert_eq!(
            parse(&Config::default().to_toml()).unwrap(),
            Config::default()
        );
    }
}
