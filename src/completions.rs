//! `crystal completions <shell>`: the script that teaches a shell to
//! complete crystal's commands and options, generated from the command line
//! crystal parses (clap), so it can't fall behind it.
//!
//! Where a command takes a session's name, bash, zsh and fish complete the
//! names of the sessions running now too, from `crystal complete-sessions`:
//! a command that asks a daemon that's running and never starts one, so a
//! Tab with no daemon is as quick as any other. clap can't say that an
//! argument is a session's name, so those arguments are given a hint no
//! other argument of crystal's has, a user's name, and what each shell's
//! script makes of that hint is swapped for the sessions. Elvish and
//! PowerShell complete commands and options only.

use clap::{Command, ValueEnum, ValueHint};
use clap_complete::Shell;

/// The shells crystal writes completions for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Target {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

impl Target {
    fn shell(self) -> Shell {
        match self {
            Target::Bash => Shell::Bash,
            Target::Zsh => Shell::Zsh,
            Target::Fish => Shell::Fish,
            Target::Elvish => Shell::Elvish,
            Target::Powershell => Shell::PowerShell,
        }
    }
}

/// The arguments that take a session's name: the subcommands they're in,
/// then the argument's id.
const SESSION_ARGS: &[(&[&str], &str)] = &[
    (&["done"], "name"),
    (&["handoff"], "name"),
    (&["report"], "name"),
    (&["answer"], "task"),
    (&["interrupt"], "task"),
    (&["tasks", "terminal"], "task"),
    (&["result"], "name"),
    (&["attach"], "name"),
    (&["send-keys"], "name"),
    (&["send"], "name"),
    (&["wait"], "name"),
    (&["events"], "name"),
    (&["read"], "name"),
    (&["clear"], "name"),
    (&["process-info"], "name"),
    (&["observe"], "name"),
    (&["control"], "name"),
    (&["rename"], "name"),
    (&["respawn"], "name"),
    (&["kill"], "name"),
    (&["archive"], "names"),
    (&["notify"], "name"),
    (&["memory", "distill"], "name"),
    (&["plugin", "run"], "session"),
    (&["plugin", "pane", "open"], "session"),
    (&["agent", "explain"], "session"),
    (&["worktree", "move"], "name"),
    (&["tab", "move"], "session"),
    (&["sidebar", "move"], "session"),
    (&["sidebar", "move"], "before"),
    (&["sidebar", "move"], "after"),
    (&["pane", "split"], "session"),
    (&["pane", "split"], "beside"),
    (&["pane", "focus"], "target"),
    (&["pane", "resize"], "name"),
    (&["pane", "swap"], "target"),
    (&["pane", "swap"], "name"),
    (&["pane", "ratio"], "session"),
    (&["pane", "close"], "session"),
    (&["pane", "zoom"], "session"),
    (&["pane", "float"], "session"),
];

/// The hint that marks an argument as a session's name in the scripts.
const SESSION_HINT: ValueHint = ValueHint::Username;

/// What lists the sessions, a name a line.
const LIST_SESSIONS: &str = "crystal complete-sessions 2>/dev/null";

/// The completion script for `target`, for the command line `cli`.
pub fn script(target: Target, cli: Command) -> String {
    let mut cli = mark_sessions(cli);
    let mut out = Vec::new();
    clap_complete::generate(target.shell(), &mut cli, "crystal", &mut out);
    let script = String::from_utf8(out).expect("clap writes UTF-8");
    match target {
        Target::Bash => script + &bash_sessions(&cli),
        Target::Zsh => zsh_sessions(&script),
        Target::Fish => fish_sessions(&script, &cli),
        Target::Elvish | Target::Powershell => script,
    }
}

/// `cli` with each of [`SESSION_ARGS`] given [`SESSION_HINT`], and without
/// its hidden commands, crystal's own, which clap's scripts would offer.
fn mark_sessions(cli: Command) -> Command {
    let mut shown = Command::new("crystal")
        .version(env!("CARGO_PKG_VERSION"))
        .args(cli.get_arguments().cloned())
        .subcommands(
            cli.get_subcommands()
                .filter(|sub| !sub.is_hide_set())
                .cloned(),
        );
    if let Some(about) = cli.get_about() {
        shown = shown.about(about.clone());
    }
    for (path, arg) in SESSION_ARGS {
        shown = mark(shown, path, arg);
    }
    shown
}

fn mark(cli: Command, path: &[&str], arg: &str) -> Command {
    match path.split_first() {
        Some((first, rest)) => {
            let rest = rest.to_vec();
            let arg = arg.to_string();
            cli.mut_subcommand(*first, move |sub| mark(sub, &rest, &arg))
        }
        None => cli.mut_arg(arg, |arg| arg.value_hint(SESSION_HINT)),
    }
}

/// Each session argument's subcommands, with their aliases, and the flags
/// that take it, none for an argument that's a positional one.
fn session_args(cli: &Command) -> Vec<(Vec<Vec<String>>, Vec<String>)> {
    SESSION_ARGS
        .iter()
        .map(|(path, id)| {
            let mut command = cli;
            let mut names = Vec::new();
            for name in *path {
                command = command
                    .find_subcommand(name)
                    .expect("a session argument's subcommand is crystal's");
                let mut spellings = vec![command.get_name().to_string()];
                spellings.extend(command.get_visible_aliases().map(str::to_string));
                names.push(spellings);
            }
            let arg = command
                .get_arguments()
                .find(|arg| arg.get_id() == *id)
                .expect("a session argument is its command's");
            let mut flags: Vec<String> = Vec::new();
            flags.extend(arg.get_short().map(|short| format!("-{short}")));
            flags.extend(arg.get_long().map(|long| format!("--{long}")));
            (names, flags)
        })
        .collect()
}

/// Every way to spell a list of subcommands, each with its aliases: `a`
/// for `attach`.
fn spellings(names: &[Vec<String>]) -> Vec<String> {
    names.iter().fold(vec![String::new()], |so_far, choices| {
        so_far
            .iter()
            .flat_map(|before| {
                choices.iter().map(move |choice| {
                    if before.is_empty() {
                        choice.clone()
                    } else {
                        format!("{before} {choice}")
                    }
                })
            })
            .collect()
    })
}

/// bash: a function around clap's that offers the sessions in place of
/// what it offers, for a word that's no option after a command that takes a
/// session's name as its first argument, or after a flag that takes one.
/// The words before, options and the values of crystal's own taken out,
/// say which command it is.
fn bash_sessions(cli: &Command) -> String {
    let mut positional = Vec::new();
    let mut flagged = Vec::new();
    for (names, flags) in session_args(cli) {
        for command in spellings(&names) {
            if flags.is_empty() {
                positional.push(format!("\"{command}\""));
            } else {
                flagged.extend(flags.iter().map(|flag| format!("\"{command} {flag}\"")));
            }
        }
    }
    format!(
        r#"
_crystal_with_sessions() {{
    _crystal "$@"
    local cur="${{COMP_WORDS[COMP_CWORD]}}" prev="${{COMP_WORDS[COMP_CWORD-1]}}"
    [[ $cur == -* ]] && return
    local word skip="" words=()
    for word in "${{COMP_WORDS[@]:1:COMP_CWORD-1}}"; do
        if [[ -n $skip ]]; then
            skip=""
        elif [[ $word == -L || $word == -S || $word == --server || $word == --socket ]]; then
            skip=1
        elif [[ $word != -* ]]; then
            words+=("$word")
        fi
    done
    local command="${{words[*]}}" found=""
    if [[ $prev == -* ]]; then
        case "$command $prev" in
            {flagged}) found=1 ;;
        esac
    else
        case "$command" in
            {positional}) found=1 ;;
        esac
    fi
    if [[ -n $found ]]; then
        COMPREPLY=($(compgen -W "$({LIST_SESSIONS})" -- "$cur"))
    fi
}}

if [[ "${{BASH_VERSINFO[0]}}" -eq 4 && "${{BASH_VERSINFO[1]}}" -ge 4 || "${{BASH_VERSINFO[0]}}" -gt 4 ]]; then
    complete -F _crystal_with_sessions -o nosort -o bashdefault -o default crystal
else
    complete -F _crystal_with_sessions -o bashdefault -o default crystal
fi
"#,
        flagged = flagged.join("|"),
        positional = positional.join("|"),
    )
}

/// zsh: what clap makes of the hint, `_users`, becomes a function that
/// offers the sessions, defined ahead of the script's last lines, which
/// run it or register it.
fn zsh_sessions(script: &str) -> String {
    let script = script.replace(":_users'", ":_crystal_sessions'");
    let function = format!(
        r#"(( $+functions[_crystal_sessions] )) ||
_crystal_sessions() {{
    local -a sessions
    sessions=(${{(f)"$({LIST_SESSIONS})"}})
    _wanted sessions expl 'session' compadd -a sessions
}}

"#
    );
    let ending = "if [ \"$funcstack[1]\" = \"_crystal\" ]; then";
    match script.rfind(ending) {
        Some(at) => format!("{}{function}{}", &script[..at], &script[at..]),
        None => script + &function,
    }
}

/// fish: a flag's values are the sessions in place of the users clap would
/// offer, and a command's own arguments are the sessions too, since clap's
/// script for fish leaves those to fish's own guess, files.
fn fish_sessions(script: &str, cli: &Command) -> String {
    let mut script = script.replace(
        "(__fish_complete_users)",
        &format!("({})", LIST_SESSIONS.replace(" 2>/dev/null", "")),
    );
    let mut seen = Vec::new();
    for (names, flags) in session_args(cli) {
        if !flags.is_empty() || seen.contains(&names) {
            continue;
        }
        let mut condition = format!("__fish_crystal_using_subcommand {}", names[0].join(" "));
        for nested in &names[1..] {
            condition.push_str(&format!(
                "; and __fish_seen_subcommand_from {}",
                nested.join(" ")
            ));
        }
        script.push_str(&format!(
            "complete -c crystal -n \"{condition}\" -f -a \"(crystal complete-sessions)\"\n"
        ));
        seen.push(names);
    }
    script
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::CommandFactory;

    /// crystal's own command line.
    fn cli() -> Command {
        crate::Cli::command()
    }

    #[test]
    fn every_shell_gets_a_script() {
        for target in Target::value_variants() {
            let script = script(*target, cli());
            assert!(script.contains("attach"), "{target:?}");
        }
    }

    #[test]
    fn zsh_offers_the_sessions_where_a_name_goes() {
        let script = script(Target::Zsh, cli());
        assert!(!script.contains("_users"), "{script}");
        assert!(script.contains(":_crystal_sessions'"), "{script}");
        let defined = script.find("_crystal_sessions() {").unwrap();
        let registered = script.rfind("compdef _crystal crystal").unwrap();
        assert!(defined < registered);
    }

    #[test]
    fn bash_offers_them_after_the_commands_and_flags_that_take_one() {
        let script = script(Target::Bash, cli());
        assert!(script.contains("\"attach\"|\"a\""), "{script}");
        assert!(script.contains("\"pane split\""), "{script}");
        assert!(script.contains("\"pane split --beside\""), "{script}");
        assert!(script.contains("\"done -n\"|\"done --name\""), "{script}");
        // `new -n` names a session that isn't there yet.
        assert!(!script.contains("\"new -n\""), "{script}");
        assert!(script.contains("complete -F _crystal_with_sessions"));
    }

    #[test]
    fn fish_offers_them_for_flags_and_arguments() {
        let script = script(Target::Fish, cli());
        assert!(!script.contains("__fish_complete_users"), "{script}");
        assert!(script.contains(
            "-n \"__fish_crystal_using_subcommand attach a\" -f -a \"(crystal complete-sessions)\""
        ));
        assert!(script.contains(
            "__fish_crystal_using_subcommand pane; and __fish_seen_subcommand_from split\" -f"
        ));
    }

    #[test]
    fn crystals_own_hidden_commands_arent_offered() {
        for target in Target::value_variants() {
            let script = script(*target, cli());
            for hidden in ["daemon", "hook", "mcp"] {
                assert!(
                    !script.contains(&format!("{hidden}:")),
                    "{target:?} {hidden}"
                );
                assert!(
                    !script.contains(&format!("\"{hidden}\"")),
                    "{target:?} {hidden}"
                );
            }
        }
    }

    #[test]
    fn a_command_and_its_aliases_are_each_a_spelling() {
        let names = vec![
            vec!["pane".to_string()],
            vec!["split".to_string(), "s".to_string()],
        ];
        assert_eq!(spellings(&names), ["pane split", "pane s"]);
    }
}
