//! crystal's plugins for the agents that take plugins rather than hooks:
//! Pi's extension, OpenCode's and Kilo Code's plugins, and Hermes Agent's.
//! Each is a file crystal writes into the agent's own directory when the
//! user asks, with `crystal integration install <agent>`, crystal's path
//! written into it, and takes out again with `uninstall`.
//!
//! A plugin runs `crystal hook <agent> --event <event>` for what its agent
//! does, with the session it's in as the hook's input, the way Claude
//! Code's hooks report: only inside a crystal session, a report at a time,
//! and never holding the agent up. The plugins are in `assets/integrations/`,
//! adapted from herdr's.
//!
//! crystal's files say so at their top, and only those are ever written
//! over or taken out; Hermes's plugin is switched on in its `config.yaml`
//! too, the rest of which stays as it was.

use crate::agent_hooks::write_whole;
use crate::integration::Standing;
use crate::shell;
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};

/// An agent crystal can give a plugin, and where.
#[derive(Debug)]
pub struct Plugin {
    /// The agent, by its id in its rules.
    pub agent: &'static str,
    /// The variables that move its directory, the first one set winning,
    /// each with where the directory is in the one it names.
    env: &'static [(&'static str, &'static str)],
    /// Its directory, in the home directory.
    home: &'static str,
    /// The files crystal writes, each where it goes in the directory and
    /// what's in it, `__CRYSTAL__` standing for crystal's path.
    files: &'static [(&'static str, &'static str)],
    /// The plugin's name in the `plugins.enabled` list of Hermes's
    /// `config.yaml`, which switches it on.
    enabled_as: Option<&'static str>,
}

const OPENCODE: &str = include_str!("../assets/integrations/opencode.js");

pub const PLUGINS: &[Plugin] = &[
    Plugin {
        agent: "pi",
        env: &[("PI_CODING_AGENT_DIR", "")],
        home: ".pi/agent",
        files: &[(
            "extensions/crystal.ts",
            include_str!("../assets/integrations/pi.ts"),
        )],
        enabled_as: None,
    },
    Plugin {
        agent: "opencode",
        env: &[("XDG_CONFIG_HOME", "opencode")],
        home: ".config/opencode",
        files: &[("plugins/crystal.js", OPENCODE)],
        enabled_as: None,
    },
    Plugin {
        agent: "kilo",
        env: &[("XDG_CONFIG_HOME", "kilo")],
        home: ".config/kilo",
        files: &[("plugin/crystal.js", OPENCODE)],
        enabled_as: None,
    },
    Plugin {
        agent: "hermes",
        env: &[("HERMES_HOME", "")],
        home: ".hermes",
        files: &[
            (
                "plugins/crystal/plugin.yaml",
                include_str!("../assets/integrations/hermes/plugin.yaml"),
            ),
            (
                "plugins/crystal/__init__.py",
                include_str!("../assets/integrations/hermes/__init__.py"),
            ),
        ],
        enabled_as: Some("crystal"),
    },
];

/// The agent `id` names, if crystal can give it a plugin.
pub fn plugin(id: &str) -> Option<&'static Plugin> {
    PLUGINS.iter().find(|plugin| plugin.agent == id)
}

/// What every file crystal writes says, in its first lines: a plugin's
/// comment that `crystal integration` wrote it, or Hermes's manifest
/// naming it.
const MARKS: &[&str] = &["`crystal integration install ", "name: crystal\n"];

impl Plugin {
    /// Its directory, as this process's environment has it.
    pub fn dir(&self) -> PathBuf {
        let moved = self.env.iter().find_map(|(name, under)| {
            let dir = std::env::var_os(name).filter(|dir| !dir.is_empty())?;
            Some(PathBuf::from(dir).join(under))
        });
        match moved {
            Some(dir) => dir,
            None => {
                let home = std::env::var_os("HOME").unwrap_or_default();
                PathBuf::from(home).join(self.home)
            }
        }
    }

    /// The plugin's main file, in `dir`: what's said about it.
    pub fn file(&self, dir: &Path) -> PathBuf {
        let (main, _) = self.files[self.files.len() - 1];
        dir.join(main)
    }

    /// Each file, where it goes in `dir`, and what it says for `crystal`.
    fn written(&self, dir: &Path, crystal: &Path) -> Vec<(PathBuf, String)> {
        let quoted = serde_json::to_string(&crystal.to_string_lossy()).unwrap_or_default();
        self.files
            .iter()
            .map(|(path, text)| {
                let text = text
                    .replace("__CRYSTAL__", &quoted)
                    .replace("__AGENT__", self.agent);
                (dir.join(path), text)
            })
            .collect()
    }

    /// How crystal's plugin stands in `dir`, for `crystal` at its path:
    /// out of date when it's there but not as this crystal writes it.
    pub fn standing(&self, dir: &Path, crystal: &Path) -> Standing {
        let mut any = false;
        let mut all = true;
        for (path, text) in self.written(dir, crystal) {
            let there = std::fs::read_to_string(&path).ok();
            any |= there.as_deref().is_some_and(is_ours);
            all &= there.as_deref() == Some(text.as_str());
        }
        if let Some(name) = self.enabled_as {
            let config = std::fs::read_to_string(dir.join("config.yaml")).unwrap_or_default();
            all &= hermes_has(&config, name);
        }
        match (any, all) {
            (true, true) => Standing::Installed,
            (true, false) => Standing::OutOfDate,
            (false, _) => Standing::NotInstalled,
        }
    }

    /// Writes crystal's plugin into `dir`, in place of any it wrote
    /// before, for `crystal` to run. The files it changed.
    pub fn install(&self, dir: &Path, crystal: &Path) -> Result<Vec<PathBuf>> {
        ensure!(
            dir.is_dir(),
            "{} isn't there: is {} installed?{}",
            shell::home_relative(dir),
            self.agent,
            self.env
                .first()
                .map(|(env, _)| format!(" (${env} says where it is)"))
                .unwrap_or_default()
        );
        let mut changed = Vec::new();
        for (path, text) in self.written(dir, crystal) {
            if let Ok(there) = std::fs::read_to_string(&path)
                && !is_ours(&there)
            {
                bail!(
                    "{} is there already, and isn't crystal's",
                    shell::home_relative(&path)
                );
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("couldn't make {}", shell::home_relative(parent)))?;
            }
            if write_whole(&path, &text)? {
                changed.push(path);
            }
        }
        if let Some(name) = self.enabled_as {
            let config = dir.join("config.yaml");
            let text = read_text(&config)?;
            let on = with_hermes_plugin(&text, name, true)
                .with_context(|| format!("couldn't read {}", shell::home_relative(&config)))?;
            if write_whole(&config, &on)? {
                changed.push(config);
            }
        }
        Ok(changed)
    }

    /// Takes crystal's plugin out of `dir`, and only crystal's. The files
    /// it changed: none when it wasn't there.
    pub fn uninstall(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let mut changed = Vec::new();
        for (path, _) in self.files {
            let path = dir.join(path);
            let Ok(there) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !is_ours(&there) {
                continue;
            }
            std::fs::remove_file(&path)
                .with_context(|| format!("couldn't remove {}", shell::home_relative(&path)))?;
            // A directory of crystal's own, left empty, goes too.
            if self.files.len() > 1
                && let Some(parent) = path.parent()
            {
                let _ = std::fs::remove_dir(parent);
            }
            changed.push(path);
        }
        if let Some(name) = self.enabled_as {
            let config = dir.join("config.yaml");
            let text = read_text(&config)?;
            let off = with_hermes_plugin(&text, name, false)
                .with_context(|| format!("couldn't read {}", shell::home_relative(&config)))?;
            if off != text {
                write_whole(&config, &off)?;
                changed.push(config);
            }
        }
        Ok(changed)
    }
}

/// Whether a file is one crystal wrote, by what it says at its top.
fn is_ours(text: &str) -> bool {
    let top: String = text.lines().take(8).collect::<Vec<_>>().join("\n") + "\n";
    MARKS.iter().any(|mark| top.contains(mark))
}

/// What's in `path`, or nothing when it isn't there.
fn read_text(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => {
            Err(err).with_context(|| format!("couldn't read {}", shell::home_relative(path)))
        }
    }
}

/// Whether Hermes's config, `text`, has the plugin `name` switched on.
fn hermes_has(text: &str, name: &str) -> bool {
    with_hermes_plugin(text, name, true).is_ok_and(|on| on == text)
}

/// Hermes's config, `text`, with the plugin `name` in its
/// `plugins.enabled` list, or out of it. Only that list's lines change:
/// the rest stays as it was, comments and all. A list written on one line,
/// `[a, b]`, is written again on one line; one item to a line, an item is
/// added after the last, as indented. Anything else there is too much to
/// change without reading YAML whole, and is left to the user.
pub fn with_hermes_plugin(text: &str, name: &str, on: bool) -> Result<String> {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let Some(plugins) = lines
        .iter()
        .position(|line| indent(line) == 0 && key(line) == Some("plugins"))
    else {
        if !on {
            return Ok(text.to_string());
        }
        let mut with = text.to_string();
        if !with.is_empty() && !with.ends_with('\n') {
            with.push('\n');
        }
        with.push_str(&format!("plugins:\n  enabled:\n    - {name}\n"));
        return Ok(with);
    };
    if !value(&lines[plugins]).is_empty() {
        bail!("its `plugins` isn't a block of settings");
    }
    let end = (plugins + 1..lines.len())
        .find(|&at| says(&lines[at]) && indent(&lines[at]) == 0)
        .unwrap_or(lines.len());
    let inner = (plugins + 1..end)
        .find(|&at| says(&lines[at]))
        .map_or(2, |at| indent(&lines[at]));
    let enabled = (plugins + 1..end)
        .find(|&at| indent(&lines[at]) == inner && key(&lines[at]) == Some("enabled"));
    let Some(enabled) = enabled else {
        if on {
            lines.insert(plugins + 1, format!("{}enabled:", " ".repeat(inner)));
            lines.insert(plugins + 2, format!("{}- {name}", " ".repeat(inner + 2)));
        }
        return Ok(joined(lines, text));
    };
    let listed = value(&lines[enabled]);
    if let Some(flow) = listed.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        let mut items: Vec<String> = flow
            .split(',')
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect();
        let had = items.iter().any(|item| unquoted(item) == name);
        match (on, had) {
            (true, true) | (false, false) => return Ok(text.to_string()),
            (true, false) => items.push(name.to_string()),
            (false, true) => items.retain(|item| unquoted(item) != name),
        }
        lines[enabled] = format!("{}enabled: [{}]", " ".repeat(inner), items.join(", "));
        return Ok(joined(lines, text));
    }
    if !matches!(listed, "" | "null" | "~") {
        bail!("its `plugins.enabled` isn't a list");
    }
    // The items: deeper than `enabled`, or as deep and each a `-`.
    let items_end = (enabled + 1..end)
        .find(|&at| {
            let line = &lines[at];
            says(line) && (indent(line) < inner || (indent(line) == inner && !is_item(line)))
        })
        .unwrap_or(end);
    let items: Vec<usize> = (enabled + 1..items_end)
        .filter(|&at| is_item(&lines[at]))
        .collect();
    let found = items
        .iter()
        .copied()
        .find(|&at| unquoted(value_of_item(&lines[at])) == name);
    match (on, found) {
        (true, Some(_)) | (false, None) => return Ok(text.to_string()),
        (true, None) => {
            let deep = items.first().map_or(inner + 2, |&at| indent(&lines[at]));
            let after = items.last().map_or(enabled + 1, |&at| at + 1);
            lines.insert(after, format!("{}- {name}", " ".repeat(deep)));
            lines[enabled] = format!("{}enabled:", " ".repeat(inner));
        }
        (false, Some(at)) => {
            lines.remove(at);
            // A list left empty goes, and `plugins` with it when that was
            // all it held, as when crystal added them.
            if items.len() == 1 {
                lines.remove(enabled);
                let left = (plugins + 1..end - 2).any(|at| says(&lines[at]));
                if !left {
                    lines.remove(plugins);
                }
            }
        }
    }
    Ok(joined(lines, text))
}

/// How far a line is indented.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// Whether a line says anything: not blank, nor a comment.
fn says(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && !line.starts_with('#')
}

/// The key a line sets, `plugins` for `plugins:`, if it sets one.
fn key(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if !says(line) || line.starts_with('-') {
        return None;
    }
    let (key, _) = line.split_once(':')?;
    Some(unquoted(key.trim()))
}

/// What a line sets its key to, without a comment after it.
fn value(line: &str) -> &str {
    let after = line.split_once(':').map_or("", |(_, after)| after);
    without_comment(after)
}

/// Whether a line is an item of a list, `- crystal`.
fn is_item(line: &str) -> bool {
    let line = line.trim_start();
    line == "-" || line.starts_with("- ")
}

/// What a list's item line holds.
fn value_of_item(line: &str) -> &str {
    without_comment(line.trim_start().trim_start_matches('-'))
}

fn without_comment(text: &str) -> &str {
    let text = match text.find(" #") {
        Some(at) => &text[..at],
        None => text,
    };
    text.trim()
}

fn unquoted(text: &str) -> &str {
    let text = text.trim();
    ['"', '\'']
        .iter()
        .find_map(|quote| text.strip_prefix(*quote)?.strip_suffix(*quote))
        .unwrap_or(text)
}

/// `lines` joined again, ending as `was` did.
fn joined(lines: Vec<String>, was: &str) -> String {
    let mut text = lines.join("\n");
    if !text.is_empty() && (was.ends_with('\n') || was.is_empty()) {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const CRYSTAL: &str = "/opt/my \"tools\"/crystal";

    #[test]
    fn each_plugin_runs_crystal_by_its_path_and_says_it_s_crystal_s() {
        let dir = tempfile::tempdir().unwrap();
        for plugin in PLUGINS {
            for (path, text) in plugin.written(dir.path(), Path::new(CRYSTAL)) {
                assert!(is_ours(&text), "{}", path.display());
                assert!(!text.contains("__CRYSTAL__"), "{}", path.display());
                assert!(!text.contains("__AGENT__"), "{}", path.display());
                if !path.ends_with("plugin.yaml") {
                    assert!(
                        text.contains(r#""/opt/my \"tools\"/crystal""#),
                        "{}",
                        path.display()
                    );
                    assert!(text.contains("CRYSTAL_SESSION"), "{}", path.display());
                }
            }
        }
        let kilo = plugin("kilo")
            .unwrap()
            .written(dir.path(), Path::new(CRYSTAL));
        assert!(kilo[0].1.contains(r#"const AGENT = "kilo";"#));
    }

    #[test]
    fn a_plugin_is_written_taken_out_and_stands_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let pi = plugin("pi").unwrap();
        let crystal = Path::new(CRYSTAL);
        assert_eq!(pi.standing(dir.path(), crystal), Standing::NotInstalled);
        let written = pi.install(dir.path(), crystal).unwrap();
        assert_eq!(written, [dir.path().join("extensions/crystal.ts")]);
        assert_eq!(pi.standing(dir.path(), crystal), Standing::Installed);
        assert!(pi.install(dir.path(), crystal).unwrap().is_empty());
        let moved = Path::new("/usr/bin/crystal");
        assert_eq!(pi.standing(dir.path(), moved), Standing::OutOfDate);
        assert_eq!(pi.install(dir.path(), moved).unwrap().len(), 1);

        assert_eq!(pi.uninstall(dir.path()).unwrap().len(), 1);
        assert!(!dir.path().join("extensions/crystal.ts").exists());
        assert!(pi.uninstall(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_file_of_the_user_s_by_the_same_name_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let opencode = plugin("opencode").unwrap();
        let file = dir.path().join("plugins/crystal.js");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "export const Mine = async () => ({});\n").unwrap();
        let err = opencode
            .install(dir.path(), Path::new(CRYSTAL))
            .unwrap_err();
        assert!(err.to_string().contains("isn't crystal's"), "{err}");
        assert!(opencode.uninstall(dir.path()).unwrap().is_empty());
        assert!(file.exists());
        assert_eq!(
            opencode.standing(dir.path(), Path::new(CRYSTAL)),
            Standing::NotInstalled
        );
    }

    #[test]
    fn hermes_s_plugin_is_switched_on_in_its_config_and_off_again() {
        let dir = tempfile::tempdir().unwrap();
        let hermes = plugin("hermes").unwrap();
        let config = dir.path().join("config.yaml");
        let own = "# mine\nmodel: x\nplugins:\n  enabled:\n    - other\n";
        std::fs::write(&config, own).unwrap();
        let crystal = Path::new(CRYSTAL);
        hermes.install(dir.path(), crystal).unwrap();
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "# mine\nmodel: x\nplugins:\n  enabled:\n    - other\n    - crystal\n"
        );
        assert!(dir.path().join("plugins/crystal/__init__.py").exists());
        assert_eq!(hermes.standing(dir.path(), crystal), Standing::Installed);

        hermes.uninstall(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), own);
        assert!(!dir.path().join("plugins/crystal").exists());
        assert_eq!(hermes.standing(dir.path(), crystal), Standing::NotInstalled);
    }

    #[test]
    fn hermes_s_list_of_plugins_is_changed_however_it_s_written() {
        let on = |text: &str| with_hermes_plugin(text, "crystal", true).unwrap();
        let off = |text: &str| with_hermes_plugin(text, "crystal", false).unwrap();
        assert_eq!(on(""), "plugins:\n  enabled:\n    - crystal\n");
        assert_eq!(
            on("model: x"),
            "model: x\nplugins:\n  enabled:\n    - crystal\n"
        );
        assert_eq!(
            on("plugins:\n    disabled: [a]\nmodel: x\n"),
            "plugins:\n    enabled:\n      - crystal\n    disabled: [a]\nmodel: x\n"
        );
        let flow = "plugins:\n  enabled: [a, 'b'] # mine\n";
        assert_eq!(on(flow), "plugins:\n  enabled: [a, 'b', crystal]\n");
        assert_eq!(off(&on(flow)), "plugins:\n  enabled: [a, 'b']\n");
        let compact = "plugins:\n  enabled:\n  - a\n  # more\n  disabled:\n  - b\n";
        assert_eq!(
            on(compact),
            "plugins:\n  enabled:\n  - a\n  - crystal\n  # more\n  disabled:\n  - b\n"
        );
        assert_eq!(off(&on(compact)), compact);
        assert_eq!(
            on("plugins:\n  enabled:\n"),
            "plugins:\n  enabled:\n    - crystal\n"
        );
        let quoted = "plugins:\n  enabled:\n    - \"crystal\"\n";
        assert_eq!(on(quoted), quoted);
        assert_eq!(off(quoted), "");
        assert_eq!(off(&on("model: x\n")), "model: x\n");
        let kept = "plugins:\n  enabled:\n    - crystal\n  disabled: [a]\nmodel: x\n";
        assert_eq!(off(kept), "plugins:\n  disabled: [a]\nmodel: x\n");
        assert_eq!(off("model: x\n"), "model: x\n");
        assert!(with_hermes_plugin("plugins: {}\n", "crystal", true).is_err());
        assert!(with_hermes_plugin("plugins:\n  enabled: all\n", "crystal", true).is_err());
    }
}
