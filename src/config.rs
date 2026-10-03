//! The settings in `~/.config/crystal/config.toml`. The file is optional,
//! and so is every setting in it; a setting left out has its default.
//!
//! A key crystal doesn't know is an error, not something to skip: a
//! setting spelled wrong would otherwise do nothing, without a word.

use crate::flows::Flow;
use crate::plugins;
use crate::profile::Profile;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Which plugins are on and off, by name: crystal's own, which are on
    /// unless switched off here, and ones the user installed, which are off
    /// until switched on. See [`crate::plugins`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, bool>,
    /// How the memory plugin learns: `[memory]` in the file.
    pub memory: MemorySettings,
    /// Saved ways to start an agent, offered first in the new-session
    /// panel: `[[profile]]` tables in the file. See [`crate::profile`].
    #[serde(rename = "profile", skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<Profile>,
    /// Chains of steps run one after another on one goal: `[[flow]]`
    /// tables in the file. See [`crate::flows`].
    #[serde(rename = "flow", skip_serializing_if = "Vec::is_empty")]
    pub flows: Vec<Flow>,
}

/// How the memory plugin learns, beyond what it's told.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    /// Once a task has closed, have a model read what it did and keep what
    /// a later session would need to know: see [`crate::distill`].
    pub distill: bool,
    /// The model that does it, as `claude --model` takes it.
    pub distill_model: String,
    /// The most it may spend on one task, in US dollars.
    pub distill_budget_usd: f64,
    /// Search by what entries mean as well as by their words, with a model
    /// run on this machine: see [`crate::embed`].
    pub embeddings: bool,
}

impl Default for MemorySettings {
    fn default() -> MemorySettings {
        MemorySettings {
            distill: true,
            distill_model: "claude-haiku-4-5".to_string(),
            distill_budget_usd: 0.25,
            embeddings: false,
        }
    }
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
            plugins: BTreeMap::new(),
            memory: MemorySettings::default(),
            profiles: Vec::new(),
            flows: Vec::new(),
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
        let config = from_text(&text).with_context(|| format!("in {}", path.display()))?;
        check_plugins(&config, &plugins::installed_names())
            .with_context(|| format!("in {}", path.display()))?;
        Ok(config)
    }

    /// The settings written as TOML, the way the file would hold them.
    pub fn to_toml(&self) -> String {
        // Our own plain struct always makes valid TOML.
        toml::to_string(self).expect("settings make TOML")
    }
}

/// Sets the setting at `keys`, like `["memory", "embeddings"]`, to `value`
/// in the config file at `path`, making the table it's in if there isn't
/// one, and keeping the rest of the file as the user wrote it, comments and
/// all: what the settings view writes. A file the change would leave
/// meaning nothing crystal knows is left as it was, with an error.
pub fn set(path: &Path, keys: &[&str], value: toml_edit::Value) -> Result<()> {
    let (last, tables) = keys.split_last().context("say which setting")?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("couldn't read {}", path.display())),
    };
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("couldn't read {}", path.display()))?;
    let mut table = document.as_table_mut();
    for key in tables {
        table = table
            .entry(key)
            .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .with_context(|| format!("`{key}` in the config file isn't a [{key}] table"))?;
    }
    // A line that's there already keeps its comment.
    match table.get_mut(last).and_then(toml_edit::Item::as_value_mut) {
        Some(said) => {
            let decor = said.decor().clone();
            *said = value;
            *said.decor_mut() = decor;
        }
        None => {
            table.insert(last, toml_edit::Item::Value(value));
        }
    }
    let new_text = document.to_string();
    from_text(&new_text).with_context(|| format!("in {}", path.display()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let unfinished = path.with_extension("toml.saving");
    std::fs::write(&unfinished, &new_text)?;
    std::fs::rename(&unfinished, path)?;
    Ok(())
}

impl ThemeName {
    pub const ALL: [ThemeName; 3] = [ThemeName::Dark, ThemeName::Light, ThemeName::Terminal];

    /// Its name, as the config file has it.
    pub fn name(self) -> &'static str {
        match self {
            ThemeName::Dark => "dark",
            ThemeName::Light => "light",
            ThemeName::Terminal => "terminal",
        }
    }

    /// The one after it, back to the first after the last.
    pub fn next(self) -> ThemeName {
        let at = ThemeName::ALL.iter().position(|theme| *theme == self);
        ThemeName::ALL[(at.unwrap_or(0) + 1) % ThemeName::ALL.len()]
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

/// Checks that every name in `[plugins]` is a plugin: one of crystal's
/// own, or one of those `installed`. A name spelled wrong would otherwise
/// switch nothing, without a word.
pub fn check_plugins(config: &Config, installed: &[String]) -> Result<()> {
    for name in config.plugins.keys() {
        let known = plugins::is_built_in(name) || installed.iter().any(|plugin| plugin == name);
        if !known {
            let built_in: Vec<&str> = plugins::BUILT_IN.iter().map(|plugin| plugin.name).collect();
            bail!(
                "`[plugins]` names `{name}`, which is neither one of crystal's plugins ({}) \
                 nor one installed in {}",
                built_in.join(", "),
                plugins::plugins_dir().display()
            );
        }
    }
    Ok(())
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
    // Memory became a plugin; say where its switch went. `[memory]` is
    // how it learns.
    if matches!(table.get("memory"), Some(toml::Value::Boolean(_))) {
        bail!("`memory` is now a plugin: put `memory = …` under a `[plugins]` line instead");
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
    for (index, flow) in config.flows.iter().enumerate() {
        flow.check(&config.profiles)?;
        if config.flows[..index]
            .iter()
            .any(|before| before.name == flow.name)
        {
            bail!("two flows are called {}", flow.name);
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flows::Step;
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
    fn plugins_are_switched_by_name() {
        let config = parse("[plugins]\nmemory = false\ngithub = true\n").unwrap();
        assert_eq!(config.plugins.get("memory"), Some(&false));
        assert_eq!(config.plugins.get("github"), Some(&true));
    }

    #[test]
    fn a_plugin_that_doesnt_exist_is_an_error_that_names_it() {
        let config = parse("[plugins]\nmemroy = false\n").unwrap();
        let err = check_plugins(&config, &[]).unwrap_err();
        assert!(format!("{err:#}").contains("memroy"), "{err:#}");
        assert!(check_plugins(&config, &["memroy".to_string()]).is_ok());
    }

    #[test]
    fn a_leftover_memory_setting_says_where_it_went() {
        let err = parse("memory = false\n").unwrap_err();
        assert!(format!("{err:#}").contains("`[plugins]`"), "{err:#}");
    }

    #[test]
    fn flows_and_their_steps_are_read_in_order() {
        let config = parse(
            r#"
[[profile]]
name = "reviewer"
agent = "claude"

[[flow]]
name = "ship"
description = "Plan, build, review"

[[flow.step]]
name = "plan"
prompt = "Plan {goal}"

[[flow.step]]
name = "build"
prompt = "Build {goal} like so: {previous}"
worktree = true

[[flow.step]]
name = "review"
profile = "reviewer"
prompt = "Review it. {feedback}"
gate = true
back_to = "build"
"#,
        )
        .unwrap();
        let ship = &config.flows[0];
        assert_eq!(ship.chain(), "plan → build → review");
        assert!(ship.steps[1].worktree && !ship.steps[1].gate);
        assert_eq!(ship.steps[2].profile.as_deref(), Some("reviewer"));
        assert_eq!(ship.steps[2].back_to.as_deref(), Some("build"));
    }

    #[test]
    fn a_flow_that_cant_run_is_an_error_that_says_why() {
        let cases = [
            ("[[flow]]\nname = \"ship\"\n", "has no steps"),
            (
                "[[flow]]\nname = \"ship\"\n[[flow.step]]\nname = \"plan\"\nprompt = \"x\"\nwait = true\n",
                "wait",
            ),
            (
                "[[flow]]\nname = \"ship\"\n[[flow.step]]\nname = \"plan\"\nprompt = \"x\"\nprofile = \"nope\"\n",
                "profile nope, which isn't there",
            ),
            (
                "[[flow]]\nname = \"x\"\n[[flow.step]]\nname = \"a\"\nprompt = \"x\"\n\
                 [[flow]]\nname = \"x\"\n[[flow.step]]\nname = \"a\"\nprompt = \"x\"\n",
                "two flows are called x",
            ),
        ];
        for (flow, expected) in cases {
            let err = parse(flow).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
    }

    #[test]
    fn a_setting_set_keeps_the_rest_of_the_file_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# mine\nnotify = true # loud\n\n[plugins]\nmemory = true\n",
        )
        .unwrap();
        set(&path, &["notify"], false.into()).unwrap();
        set(&path, &["memory", "embeddings"], true.into()).unwrap();
        set(&path, &["theme"], "light".into()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with("# mine\nnotify = false # loud\n"),
            "{text}"
        );
        assert!(text.contains("[memory]\nembeddings = true\n"), "{text}");
        let config = from_text(&text).unwrap();
        assert!(!config.notify && config.memory.embeddings);
        assert_eq!(config.theme, ThemeName::Light);

        // What crystal wouldn't take is never written.
        assert!(set(&path, &["theme"], "pink".into()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn the_themes_go_round() {
        assert_eq!(ThemeName::Dark.next(), ThemeName::Light);
        assert_eq!(ThemeName::Terminal.next(), ThemeName::Dark);
        assert_eq!(ThemeName::Light.name(), "light");
    }

    #[test]
    fn memory_learns_by_its_own_table() {
        let config = parse("[memory]\ndistill = false\n").unwrap();
        assert!(!config.memory.distill);
        assert_eq!(config.memory.distill_model, "claude-haiku-4-5");
        assert_eq!(config.memory.distill_budget_usd, 0.25);
        assert!(!config.memory.embeddings, "off until the model is wanted");
        let config = parse("[memory]\ndistill_model = \"sonnet\"\ndistill_budget_usd = 1\n");
        let config = config.unwrap();
        assert_eq!(config.memory.distill_model, "sonnet");
        assert_eq!(config.memory.distill_budget_usd, 1.0);
        assert!(parse("[memory]\ndistil = false\n").is_err());
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
            plugins: BTreeMap::from([("memory".to_string(), false)]),
            memory: MemorySettings {
                distill: false,
                distill_model: "claude-sonnet-5-5".into(),
                distill_budget_usd: 0.5,
                embeddings: true,
            },
            profiles: vec![Profile {
                name: "review".into(),
                description: Some("A second pair of eyes".into()),
                agent: "claude".into(),
                model: Some("opus".into()),
                effort: Some("high".into()),
                mode: Some("plan".into()),
                args: vec!["--verbose".into()],
                prompt: Some("Review it.".into()),
                instructions: Some("Be brief.".into()),
                start_in: Some(StartIn::Worktree),
            }],
            flows: vec![Flow {
                name: "ship".into(),
                description: Some("Plan, then build".into()),
                steps: vec![
                    Step {
                        name: "plan".into(),
                        profile: Some("review".into()),
                        prompt: "Plan {goal}".into(),
                        worktree: false,
                        gate: true,
                        back_to: None,
                    },
                    Step {
                        name: "build".into(),
                        profile: None,
                        prompt: "Build it:\n{previous}".into(),
                        worktree: true,
                        gate: false,
                        back_to: None,
                    },
                ],
            }],
        };
        assert_eq!(parse(&config.to_toml()).unwrap(), config);
        assert_eq!(
            parse(&Config::default().to_toml()).unwrap(),
            Config::default()
        );
    }
}
