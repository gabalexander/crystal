//! The settings in `~/.config/crystal/config.toml`. The file is optional,
//! and so is every setting in it; a setting left out has its default.
//!
//! A key crystal doesn't know is an error, not something to skip: a
//! setting spelled wrong would otherwise do nothing, without a word.

use crate::profile::Profile;
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
    /// Show Claude Code what the project's earlier sessions learned when a
    /// session starts: see [`crate::memory`].
    pub memory: bool,
    /// Saved ways to start an agent, offered first in the new-session
    /// panel: `[[profile]]` tables in the file. See [`crate::profile`].
    #[serde(rename = "profile", skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<Profile>,
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
            memory: true,
            profiles: Vec::new(),
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
        from_text(&text).with_context(|| format!("in {}", path.display()))
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

/// The settings `text`, a config file's contents, holds; or an error that
/// says what doesn't make sense in it.
pub fn from_text(text: &str) -> Result<Config> {
    let table: toml::Table = toml::from_str(text)?;
    // Profiles were called presets for a while; say so rather than only
    // that `preset` is a key crystal doesn't know.
    if table.contains_key("preset") {
        bail!("`[[preset]]` tables are now `[[profile]]`: rename them in the file");
    }
    let config: Config = table.try_into()?;
    for profile in &config.profiles {
        profile.check()?;
    }
    let mut names: Vec<&str> = config.profiles.iter().map(|p| p.name.as_str()).collect();
    names.sort_unstable();
    if let Some(pair) = names.windows(2).find(|pair| pair[0] == pair[1]) {
        bail!("two profiles are called {}", pair[0]);
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::StartIn;

    fn parse(text: &str) -> Result<Config> {
        from_text(text)
    }

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
    fn profiles_are_read_in_order() {
        let config = parse(
            r#"
[[profile]]
name = "review"
description = "A second pair of eyes"
agent = "claude"
mode = "plan"
prompt = "Review the diff on this branch."
instructions = "Point out risks before style."
where = "here"

[[profile]]
name = "fast"
agent = "codex"
model = "gpt-6-luna"
args = ["--search"]
where = "worktree"
"#,
        )
        .unwrap();
        let names: Vec<&str> = config.profiles.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["review", "fast"]);
        assert_eq!(config.profiles[0].mode.as_deref(), Some("plan"));
        assert_eq!(config.profiles[0].start_in, Some(StartIn::Here));
        assert_eq!(config.profiles[1].args, ["--search"]);
        assert_eq!(config.profiles[1].start_in, Some(StartIn::Worktree));
    }

    #[test]
    fn a_profile_that_cant_start_is_an_error_that_says_why() {
        let cases = [
            (
                "name = \"x\"\nagent = \"vim\"",
                "doesn't know the agent vim",
            ),
            (
                "name = \"x\"\nagent = \"claude\"\nmode = \"yolo\"",
                "isn't a mode of Claude Code",
            ),
            (
                "name = \"x\"\nagent = \"claude\"\nflavor = \"mint\"",
                "flavor",
            ),
            ("name = \"x\"\nagent = \"claude\"\nwhere = \"moon\"", "moon"),
            (
                "name = \"x\"\nagent = \"claude\"\n[[profile]]\nname = \"x\"\nagent = \"codex\"",
                "two profiles are called x",
            ),
        ];
        for (profile, expected) in cases {
            let err = parse(&format!("[[profile]]\n{profile}\n")).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
    }

    #[test]
    fn a_leftover_preset_says_its_now_a_profile() {
        let err = parse("[[preset]]\nname = \"x\"\nagent = \"claude\"\n").unwrap_err();
        assert!(
            format!("{err:#}").contains("`[[preset]]` tables are now `[[profile]]`"),
            "{err:#}"
        );
    }

    #[test]
    fn the_settings_written_out_read_back_the_same() {
        let config = Config {
            notify: false,
            notify_command: Some("say \"$CRYSTAL_NOTICE\"".into()),
            new_session: "codex --model o3".into(),
            theme: ThemeName::Terminal,
            memory: false,
            profiles: vec![Profile {
                name: "review".into(),
                description: Some("A second pair of eyes".into()),
                agent: "claude".into(),
                model: Some("opus".into()),
                mode: Some("plan".into()),
                args: vec!["--verbose".into()],
                prompt: Some("Review it.".into()),
                instructions: Some("Be brief.".into()),
                start_in: Some(StartIn::Worktree),
            }],
        };
        assert_eq!(parse(&config.to_toml()).unwrap(), config);
        assert_eq!(
            parse(&Config::default().to_toml()).unwrap(),
            Config::default()
        );
    }
}
