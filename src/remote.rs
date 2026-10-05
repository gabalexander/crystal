//! `crystal ssh`: crystal on another machine, over the user's own ssh.
//!
//! Everything goes through the `ssh` command, so the user's `~/.ssh/config`,
//! keys and agent work as they always do, and crystal never sees a password
//! or a key. Two connections are made: one to find crystal on the other
//! machine (and install it there if the user says so), then one to run it.
//! The crystal over there has its own daemon and its own sessions; nothing
//! here talks to them except through that second connection.

use crate::output::{err, errln};
use crate::shell;
use anyhow::{Context, Result, bail};
use std::ffi::OsString;
use std::io::{BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};

/// The install script, run on the other machine to put crystal there. It
/// also restarts a daemon that's already running there, so an upgrade
/// takes over at once.
const INSTALL: &str =
    "curl -fsSL https://raw.githubusercontent.com/gabalexander/crystal/master/install.sh | sh";

/// What the other machine runs to find crystal. A shell that ssh starts
/// for a command reads less of the user's setup than a terminal does, so
/// `~/.local/bin`, where the install script puts crystal, is often not on
/// its PATH; it's tried after PATH, and so is `~/.cargo/bin`. The Crystal
/// programming language has a `crystal` too, so a candidate counts only if
/// its version says it's this crystal. Prints where crystal is and its
/// version, or `missing`.
const FIND_CRYSTAL: &str = r#"
for c in "$(command -v crystal)" "$HOME/.local/bin/crystal" "$HOME/.cargo/bin/crystal"; do
    [ -n "$c" ] && [ -x "$c" ] || continue
    v=$("$c" --version 2>/dev/null) || continue
    case "$v" in "crystal "*) echo "$c"; echo "$v"; exit 0 ;; esac
done
echo missing
"#;

/// Crystal on the other machine.
#[derive(Debug, PartialEq, Eq)]
struct RemoteCrystal {
    path: String,
    version: String,
}

/// Crystal, if it was found on the other machine.
#[derive(Debug, PartialEq, Eq)]
enum Found {
    Missing,
    At(RemoteCrystal),
}

/// Runs `args`, a crystal command, on `destination`, or its TUI when there
/// are none. With `install`, crystal is installed or upgraded there without
/// asking. Returns the exit code of the remote command, for crystal to exit
/// with.
pub fn run(destination: &str, args: &[String], install: bool) -> Result<i32> {
    let mut crystal = match find(destination)? {
        Found::At(crystal) => crystal,
        Found::Missing => {
            let question = format!(
                "crystal isn't installed on {destination}. Install it there with its install script?"
            );
            if !install && !ask(&question)? {
                bail!(
                    "crystal isn't installed on {destination}: \
                     run `crystal ssh --install {destination}` to install it there"
                );
            }
            install_on(destination)?
        }
    };

    // The two never talk to each other, so a different version still
    // works; it's only worth knowing, and upgrading if the user wants.
    let ours = env!("CARGO_PKG_VERSION");
    if crystal.version != ours {
        errln!(
            "crystal: crystal on {destination} is {}, and this one is {ours}",
            crystal.version
        );
        let question = format!("Upgrade crystal on {destination} to the latest release?");
        if install || ask(&question)? {
            crystal = install_on(destination)?;
        }
    }

    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let status = Command::new(ssh_program())
        .args(ssh_args(
            destination,
            &remote_command(&crystal.path, args),
            tty,
        ))
        .status()
        .context("couldn't run ssh")?;
    Ok(status.code().unwrap_or(1))
}

/// Looks for crystal on `destination`.
fn find(destination: &str) -> Result<Found> {
    let output = Command::new(ssh_program())
        .args(ssh_args(destination, &in_sh(FIND_CRYSTAL), false))
        .stderr(Stdio::inherit())
        .output()
        .context("couldn't run ssh")?;
    if !output.status.success() {
        bail!("couldn't reach {destination} with ssh");
    }
    parse_found(&String::from_utf8_lossy(&output.stdout))
}

/// Runs the install script on `destination`, then finds what it installed.
fn install_on(destination: &str) -> Result<RemoteCrystal> {
    errln!("crystal: installing crystal on {destination}");
    let status = Command::new(ssh_program())
        .args(ssh_args(destination, &in_sh(INSTALL), false))
        .status()
        .context("couldn't run ssh")?;
    if !status.success() {
        bail!("installing crystal on {destination} failed");
    }
    match find(destination)? {
        Found::At(crystal) => Ok(crystal),
        Found::Missing => bail!("the install script didn't leave crystal on {destination}"),
    }
}

/// What [`FIND_CRYSTAL`] printed: `missing`, or crystal's path on one line
/// and `crystal <version>` on the next.
fn parse_found(output: &str) -> Result<Found> {
    let mut lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next().context("ssh printed nothing")?;
    if first == "missing" {
        return Ok(Found::Missing);
    }
    let version = lines
        .next()
        .and_then(|line| line.strip_prefix("crystal "))
        .with_context(|| format!("{first} didn't say which crystal it is"))?;
    Ok(Found::At(RemoteCrystal {
        path: first.to_string(),
        version: version.to_string(),
    }))
}

/// The command line for the other machine's shell: crystal at `path`, and
/// `args`, each quoted, so that spaces, quotes and `$` arrive as typed.
fn remote_command(path: &str, args: &[String]) -> String {
    let mut words = vec![shell::quote(path)];
    words.extend(args.iter().map(|arg| shell::quote(arg)));
    words.join(" ")
}

/// `script`, to be run by `sh` whatever the user's own shell is over there.
fn in_sh(script: &str) -> String {
    format!("sh -c {}", shell::quote(script))
}

/// The arguments for ssh. ssh joins everything after the destination into
/// one command line, so the command goes as one argument, already quoted.
/// With `tty`, ssh gives the command a terminal, which the TUI and
/// attaching need; a run whose output goes to a pipe shouldn't have one.
fn ssh_args(destination: &str, command: &str, tty: bool) -> Vec<String> {
    let mut args = Vec::new();
    if tty {
        args.push("-t".to_string());
    }
    // `--` so that a destination can never be taken for an ssh option.
    args.extend([
        "--".to_string(),
        destination.to_string(),
        command.to_string(),
    ]);
    args
}

/// `ssh`, or the command `CRYSTAL_SSH` names instead.
fn ssh_program() -> OsString {
    std::env::var_os("CRYSTAL_SSH")
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| "ssh".into())
}

/// Asks the user a yes-or-no question on the terminal; no is the answer
/// when there's no one at a terminal to ask.
fn ask(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(false);
    }
    err!("{question} [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn the_command_quotes_what_a_shell_would_split() {
        let args = words(&["new", "-d", "claude", "it's $HOME"]);
        assert_eq!(
            remote_command("crystal", &args),
            r"crystal new -d claude 'it'\''s $HOME'"
        );
    }

    #[test]
    fn a_path_with_a_space_is_quoted_too() {
        assert_eq!(
            remote_command("/home/a b/.local/bin/crystal", &words(&["ls"])),
            "'/home/a b/.local/bin/crystal' ls"
        );
    }

    #[test]
    fn no_args_runs_the_tui() {
        assert_eq!(remote_command("crystal", &[]), "crystal");
    }

    #[test]
    fn a_terminal_is_asked_for_only_when_there_is_one() {
        assert_eq!(
            ssh_args("box", "crystal", true),
            ["-t", "--", "box", "crystal"]
        );
        assert_eq!(
            ssh_args("box", "crystal ls", false),
            ["--", "box", "crystal ls"]
        );
    }

    #[test]
    fn crystal_is_found_with_its_path_and_version() {
        let found = parse_found("/home/me/.local/bin/crystal\ncrystal 0.1.0\n").unwrap();
        let expected = RemoteCrystal {
            path: "/home/me/.local/bin/crystal".into(),
            version: "0.1.0".into(),
        };
        assert_eq!(found, Found::At(expected));
    }

    #[test]
    fn missing_crystal_says_so() {
        assert_eq!(parse_found("missing\n").unwrap(), Found::Missing);
    }

    #[test]
    fn output_that_isnt_crystals_is_an_error() {
        assert!(parse_found("").is_err());
        assert!(parse_found("/usr/bin/crystal\nCrystal 1.11.2 [LLVM 15]\n").is_err());
    }

    #[test]
    fn scripts_run_in_sh_whatever_the_login_shell() {
        assert_eq!(in_sh("echo \"$HOME\""), r#"sh -c 'echo "$HOME"'"#);
    }
}
