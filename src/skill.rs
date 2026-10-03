//! `crystal skill`: a Claude Code skill that teaches an agent to drive
//! crystal, so that it can start other agents, hand them work, wait for
//! them and answer their questions. The skill is a file built into crystal;
//! `--install` puts it where Claude Code looks for the user's skills.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// The skill, as written in the repository's `skill/SKILL.md`.
pub const SKILL: &str = include_str!("../skill/SKILL.md");

/// What installing the skill comes to, given what's at its path already.
#[derive(Debug, PartialEq, Eq)]
pub enum Install {
    /// Nothing is there, or `--force` was given: write the skill.
    Write,
    /// The same skill is there already.
    UpToDate,
    /// A different file is there, which may hold the user's own edits.
    Refuse,
}

pub fn print() {
    print!("{SKILL}");
}

/// Writes the skill to Claude Code's skills directory, and says where.
pub fn install(force: bool) -> Result<()> {
    let config_dir = claude_config_dir(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("HOME"),
    )
    .context("can't tell where Claude Code keeps its skills: HOME isn't set")?;
    let path = skill_path(&config_dir);
    let existing = fs::read_to_string(&path).ok();
    match decide(existing.as_deref(), force) {
        Install::UpToDate => println!("the skill is already in {}", path.display()),
        Install::Refuse => bail!(
            "{} has been changed; run `crystal skill --install --force` to write over it",
            path.display()
        ),
        Install::Write => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&path, SKILL)
                .with_context(|| format!("couldn't write {}", path.display()))?;
            println!("installed the skill in {}", path.display());
        }
    }
    Ok(())
}

/// Claude Code's config directory: `$CLAUDE_CONFIG_DIR` when it's set,
/// as Claude Code itself reads it, or else `~/.claude`.
pub fn claude_config_dir(
    from_env: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    match from_env {
        Some(dir) if !dir.is_empty() => Some(PathBuf::from(dir)),
        _ => home.map(|home| PathBuf::from(home).join(".claude")),
    }
}

/// Where the skill goes in Claude Code's config directory.
fn skill_path(config_dir: &Path) -> PathBuf {
    config_dir.join("skills").join("crystal").join("SKILL.md")
}

/// Whether to write the skill over `existing`, the file at its path if
/// there is one. A file that isn't this skill may hold the user's edits,
/// so it's only written over when they say so.
fn decide(existing: Option<&str>, force: bool) -> Install {
    match existing {
        Some(text) if text == SKILL => Install::UpToDate,
        Some(_) if !force => Install::Refuse,
        _ => Install::Write,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_skill_goes_in_claude_codes_config_directory() {
        let dir = claude_config_dir(Some("/opt/claude".into()), Some("/home/ann".into()));
        assert_eq!(
            skill_path(&dir.unwrap()),
            Path::new("/opt/claude/skills/crystal/SKILL.md")
        );
    }

    #[test]
    fn without_claude_config_dir_it_goes_under_home() {
        let dir = claude_config_dir(None, Some("/home/ann".into()));
        assert_eq!(dir, Some(PathBuf::from("/home/ann/.claude")));
        let empty = claude_config_dir(Some("".into()), Some("/home/ann".into()));
        assert_eq!(empty, Some(PathBuf::from("/home/ann/.claude")));
        assert_eq!(claude_config_dir(None, None), None);
    }

    #[test]
    fn a_changed_file_is_only_written_over_when_forced() {
        assert_eq!(decide(None, false), Install::Write);
        assert_eq!(decide(Some(SKILL), false), Install::UpToDate);
        assert_eq!(decide(Some("my own notes"), false), Install::Refuse);
        assert_eq!(decide(Some("my own notes"), true), Install::Write);
    }

    #[test]
    fn the_skill_names_itself_for_claude_code() {
        assert!(SKILL.starts_with("---\nname: crystal\ndescription: "));
    }
}
