//! A project's own commands: `run`, which runs it in a terminal of its own,
//! like a dev server, and `open`, which opens a worktree of it somewhere
//! else, like an editor. A project says them in `.crystal/project.toml`, in
//! the worktree or else in its main worktree, and the config file's
//! `[[project]]` tables take their place. Both are shell lines, run in the
//! worktree's directory.

use crate::config::Config;
use crate::protocol::SessionInfo;
use crate::shell;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Where a worktree keeps its project's commands.
pub const FILE: &str = ".crystal/project.toml";

/// The commands, as a project file or a `[[project]]` table says them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    run: Option<String>,
    open: Option<String>,
}

/// What runs a worktree and what opens it, if anything says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Commands {
    pub run: Option<String>,
    pub open: Option<String>,
}

/// Which of the two commands: what runs a project, or what opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Run,
    Open,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Run => "run",
            Verb::Open => "open",
        }
    }
}

impl Commands {
    /// The commands for the worktree at `worktree`, of the project whose
    /// main worktree is `project`: each from the config's `[[project]]` for
    /// it, or else the worktree's `.crystal/project.toml`, or else the main
    /// worktree's. A file that can't be read is an error that names it.
    pub fn of(config: &Config, worktree: &Path, project: &Path) -> Result<Commands> {
        let real = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let configured = config
            .projects
            .iter()
            .find(|listed| real(&shell::expand_home(&listed.path)) == real(project));
        let mut commands = Commands {
            run: configured.and_then(|listed| listed.run.clone()),
            open: configured.and_then(|listed| listed.open.clone()),
        };
        let mut dirs = vec![worktree];
        if real(worktree) != real(project) {
            dirs.push(project);
        }
        for dir in dirs {
            let file = read(&dir.join(FILE))?;
            commands.run = commands.run.or(file.run);
            commands.open = commands.open.or(file.open);
        }
        commands.run = commands.run.filter(|line| !line.trim().is_empty());
        commands.open = commands.open.filter(|line| !line.trim().is_empty());
        Ok(commands)
    }

    /// The command for `which`, or an error that says where to put one.
    pub fn line(&self, which: Verb, project_name: &str) -> Result<&str> {
        let line = match which {
            Verb::Run => self.run.as_deref(),
            Verb::Open => self.open.as_deref(),
        };
        match line {
            Some(line) => Ok(line),
            None => bail!(
                "{project_name} has no {word} command: put `{word} = \"…\"` in its {FILE}, \
                 or in a [[project]] table in the config file",
                word = which.word()
            ),
        }
    }
}

/// The project file at `file`, or none when there isn't one.
fn read(file: &Path) -> Result<File> {
    match fs::read_to_string(file) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("in {}", file.display())),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(File::default()),
        Err(err) => Err(err).with_context(|| format!("couldn't read {}", file.display())),
    }
}

/// The command a session runs `line` with: the user's shell, which finds
/// what's on their `PATH`, or `sh`.
pub fn shell_command(line: &str) -> Vec<String> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string());
    vec![shell, "-c".to_string(), line.to_string()]
}

/// What the run session of the worktree at `worktree` is called, before a
/// number makes it one that isn't taken: `run-` and the worktree's
/// directory, `run-app` or `run-fix-login`.
pub fn run_name(worktree: &Path) -> String {
    let dir = worktree
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir: String = dir
        .chars()
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .collect();
    if dir.is_empty() {
        "run".to_string()
    } else {
        format!("run-{dir}")
    }
}

/// Whether `session` is the run session of the worktree at `worktree`,
/// whose run command is `line`: it runs that line there.
pub fn is_run_of(session: &SessionInfo, worktree: &Path, line: &str) -> bool {
    let there = session.worktree.as_ref().map(|w| w.path.as_path()) == Some(worktree);
    there && session.command == shell_command(line)
}

/// Runs `line`, a project's open command, in the worktree at `worktree`,
/// in the background and in a process group of its own, so that it lives
/// on however crystal ends; what it prints is thrown away. Comes back once
/// it has started.
pub fn open(line: &str, worktree: &Path) -> Result<()> {
    let argv = shell_command(line);
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("couldn't run {line}"))?;
    // Reaped on a thread of its own, so it never lingers as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProjectSettings;

    fn write(dir: &Path, text: &str) {
        fs::create_dir_all(dir.join(".crystal")).unwrap();
        fs::write(dir.join(FILE), text).unwrap();
    }

    #[test]
    fn a_worktrees_own_file_comes_before_the_main_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let (main, linked) = (dir.path().join("app"), dir.path().join("fix"));
        write(&main, "run = \"npm run dev\"\nopen = \"code .\"\n");
        write(&linked, "run = \"npm run dev -- --port 3001\"\n");
        let config = Config::default();
        let commands = Commands::of(&config, &linked, &main).unwrap();
        assert_eq!(commands.run.as_deref(), Some("npm run dev -- --port 3001"));
        assert_eq!(commands.open.as_deref(), Some("code ."));
        let commands = Commands::of(&config, &main, &main).unwrap();
        assert_eq!(commands.run.as_deref(), Some("npm run dev"));
    }

    #[test]
    fn the_config_takes_the_place_of_the_projects_file() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("app");
        write(&main, "run = \"make dev\"\nopen = \"code .\"\n");
        let config = Config {
            projects: vec![ProjectSettings {
                path: main.clone(),
                run: Some("cargo run".to_string()),
                open: None,
            }],
            ..Config::default()
        };
        let commands = Commands::of(&config, &main, &main).unwrap();
        assert_eq!(commands.run.as_deref(), Some("cargo run"));
        assert_eq!(commands.open.as_deref(), Some("code ."));
    }

    #[test]
    fn a_project_with_no_command_says_where_one_goes() {
        let dir = tempfile::tempdir().unwrap();
        let commands = Commands::of(&Config::default(), dir.path(), dir.path()).unwrap();
        assert_eq!(commands, Commands::default());
        let err = commands.line(Verb::Run, "app").unwrap_err().to_string();
        assert!(err.contains("app has no run command"), "{err}");
        assert!(err.contains(FILE), "{err}");
    }

    #[test]
    fn a_file_that_doesnt_make_sense_is_named() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "runn = \"make\"\n");
        let err = Commands::of(&Config::default(), dir.path(), dir.path()).unwrap_err();
        assert!(format!("{err:#}").contains("project.toml"), "{err:#}");
    }

    #[test]
    fn a_run_session_is_named_after_its_worktree() {
        assert_eq!(run_name(Path::new("/code/app")), "run-app");
        assert_eq!(
            run_name(Path::new("/code/app.worktrees/fix-login")),
            "run-fix-login"
        );
        assert_eq!(run_name(Path::new("/")), "run");
    }
}
