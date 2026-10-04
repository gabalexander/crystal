//! `crystal skill`: a Claude Code skill that teaches an agent to drive
//! crystal, so that it can start other agents, hand them work, wait for
//! them and answer their questions. The skill is a file built into crystal;
//! `--install` puts it where Claude Code looks for the user's skills, and
//! the daemon, as it starts, brings a copy an earlier crystal put there up
//! to date, unless the user has changed it.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

/// The skill, as written in the repository's `skill/SKILL.md`.
pub const SKILL: &str = include_str!("../skill/SKILL.md");

/// The SHA-256 of every skill crystal has shipped, this one's included: a
/// file at the skill's path with one of these is a copy crystal wrote that
/// nobody has changed since, so it can be written over. A change to
/// `skill/SKILL.md` adds its hash here, which a test checks.
const SHIPPED: &[&str] = &[
    "4437c3be3fcb799e409750aca0de3c16b43821f093b2548e4b05ad278f8803ec",
    "4fd05e10003a08fcd6da45c2b4bd99249c18d1e556024b67ece4a28abd839434",
    "8f50dd76a639125d2159082e5546df993d13e4296277c9a8fd588e8940cce4bc",
    "ec3c8498f1520fe727f285394439a749b9402fda1660389029b6a2a92d954dab",
    "3e85415be5d034c0861bc76ea32e2958da23ec0530ff081fa09c8af3fbf86497",
    "f1bb028d6b0d5c739504ddc1e79fdcdc5761fcbe80c4f0993d349a5ee33924a5",
    "9049ada1dd83cb9047dafbf570da2d0479e715ff0060a1eb5957b30f233a83ec",
    "a841c270796bb9e84e65ef578c5630702e3edc92a5f902a5cb4d497ded03abb2",
    "c01d214e229e3e0aaa8fb170fd3b1b7e2235dc038c3a62154326bd96e21a2273",
    "5db71591fc37ca852c64542c79426f24dd5cbaeb6fd67a113a6db5848a192591",
    "6b69e3a4a3d95ea0af92aabeca854dad51789edfcaa3fb24b6e75cbf617d2d1b",
    "5b08940ae1a61da37e62256c40eebc9cc2a41ebb8db08ce208ebc202161a8fc6",
    "f6d72992520ee1c6999f695f2d0c10ab843b249ab700c2f52da5a3f5e976f966",
    "91e48dfad477aa973467660affb589068d6fa882063387fcea3ceb0282d56908",
    "5a20cbb3702cdc188ba50083517f706b40c3a8d6d06176a4b100371747c0fc77",
    "8a65f64ea50424c2e344d8351c30a92f6132f75b6a26622656e6ae5ad0d5dfc4",
    "d2cd00ef6b6d7f24ee4c6a53e4e1888ba76bea139e9c4c1f9ba1cf61394a68fc",
    "cec4e7543e5e2ec0474a0552e61a9f89fce6b66dbdb0f29bada7750caf9a0f47",
    "1e8cc5f1f7bf7f067e2ce59b9a8c6a30ab9f8e810f4c9ea4355fae4c36a766df",
    "9ec77c1af72360799983c25b4c0c3599516c6616f8b7d2db1c09fb955fd3ab13",
    "3876d72ee94ffb086af49cc8bb816eb8cfb0967cea0079cefaa98738136f6dd5",
    "52d0012cb7fedadb9de8d4b33a4acf9b297886d128de24cba4a528c64486cd4e",
    "f0729d9c82c8f87294f12e5212b7fdb32709c3e81e2c41f3b237c9d2b9dfc26a",
    "66ae317c12243f2c308c01b16acce038b77f4785fd3582f38126fd949a803193",
];

/// What installing the skill comes to, given what's at its path already.
#[derive(Debug, PartialEq, Eq)]
pub enum Install {
    /// Nothing is there, or `--force` was given: write the skill.
    Write,
    /// An earlier crystal's skill is there, as it wrote it: write this one.
    Update,
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
    let path = path()?;
    let existing = fs::read_to_string(&path).ok();
    match decide(existing.as_deref(), force, SHIPPED) {
        Install::UpToDate => println!("the skill is already in {}", path.display()),
        Install::Refuse => bail!(
            "{} has been changed; run `crystal skill --install --force` to write over it",
            path.display()
        ),
        Install::Write => {
            write(&path)?;
            println!("installed the skill in {}", path.display());
        }
        Install::Update => {
            write(&path)?;
            println!("updated the skill in {}", path.display());
        }
    }
    Ok(())
}

/// Brings the skill up to date where an earlier crystal installed it and
/// nobody has changed it since, and returns where. Nothing is written where
/// it was never installed, or has been changed.
pub fn refresh() -> Result<Option<PathBuf>> {
    let path = path()?;
    Ok(refresh_at(&path, SHIPPED)?.then_some(path))
}

/// Writes the skill at `path` when what's there is one of the skills
/// `shipped` names, other than this one, and says whether it did.
fn refresh_at(path: &Path, shipped: &[&str]) -> Result<bool> {
    let Ok(existing) = fs::read_to_string(path) else {
        return Ok(false);
    };
    if decide(Some(&existing), false, shipped) != Install::Update {
        return Ok(false);
    }
    write(path)?;
    Ok(true)
}

/// Where the skill goes, in the Claude Code config directory this
/// process's environment names.
fn path() -> Result<PathBuf> {
    let config_dir = claude_config_dir(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("HOME"),
    )
    .context("can't tell where Claude Code keeps its skills: HOME isn't set")?;
    Ok(skill_path(&config_dir))
}

fn write(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, SKILL).with_context(|| format!("couldn't write {}", path.display()))
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
/// there is one, given the hashes of the skills crystal `shipped`. A file
/// that isn't one of them may hold the user's edits, so it's only written
/// over when they say so.
fn decide(existing: Option<&str>, force: bool, shipped: &[&str]) -> Install {
    match existing {
        None => Install::Write,
        Some(text) if text == SKILL => Install::UpToDate,
        Some(text) if shipped.contains(&sha256(text).as_str()) => Install::Update,
        Some(_) if force => Install::Write,
        Some(_) => Install::Refuse,
    }
}

/// The SHA-256 of `text`, in hex.
fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
        assert_eq!(decide(None, false, SHIPPED), Install::Write);
        assert_eq!(decide(Some(SKILL), false, SHIPPED), Install::UpToDate);
        assert_eq!(
            decide(Some("my own notes"), false, SHIPPED),
            Install::Refuse
        );
        assert_eq!(decide(Some("my own notes"), true, SHIPPED), Install::Write);
    }

    #[test]
    fn an_earlier_crystal_s_skill_is_brought_up_to_date() {
        let shipped = sha256("the skill crystal 0.1 wrote");
        let shipped = [shipped.as_str()];
        assert_eq!(
            decide(Some("the skill crystal 0.1 wrote"), false, &shipped),
            Install::Update
        );
        assert_eq!(
            decide(
                Some("the skill crystal 0.1 wrote, and mine"),
                false,
                &shipped
            ),
            Install::Refuse
        );
    }

    #[test]
    fn refreshing_writes_only_over_an_earlier_crystal_s_skill() {
        let dir = tempfile::tempdir().unwrap();
        let path = skill_path(dir.path());
        let shipped = sha256("the skill crystal 0.1 wrote");
        let shipped = [shipped.as_str()];
        // Never installed: it stays that way.
        assert!(!refresh_at(&path, &shipped).unwrap());
        assert!(!path.exists());

        write(&path).unwrap();
        fs::write(&path, "the skill crystal 0.1 wrote").unwrap();
        assert!(refresh_at(&path, &shipped).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), SKILL);
        assert!(!refresh_at(&path, &shipped).unwrap(), "up to date already");

        fs::write(&path, "my own notes").unwrap();
        assert!(!refresh_at(&path, &shipped).unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "my own notes");
    }

    #[test]
    fn hashes_are_sha_256_in_hex() {
        assert_eq!(
            sha256(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_skill_is_among_those_shipped() {
        let hash = sha256(SKILL);
        assert!(
            SHIPPED.contains(&hash.as_str()),
            "skill/SKILL.md has changed: add \"{hash}\" to SHIPPED in src/skill.rs, so that the \
             next crystal knows this copy as its own and brings it up to date"
        );
    }

    #[test]
    fn the_skill_names_itself_for_claude_code() {
        assert!(SKILL.starts_with("---\nname: crystal\ndescription: "));
    }
}
