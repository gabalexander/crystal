//! What's in front in a session's terminal: the program its keys go to, and
//! whether that's an agent, a shell, or something else. A session started
//! as a shell can have Claude Code in front a moment later, and the shell
//! back once Claude exits, so this is looked at again as the session runs.
//!
//! The terminal itself knows: its foreground process group is the job the
//! shell put in front, and that group's leader is the program. Its
//! executable and its arguments say what it is. Neither is always named
//! after the agent: Claude Code's own binary is named after its version,
//! and an agent installed with npm runs as `node` with the agent's package
//! as its script. So [`classify`] looks at all of them. A wrapper that hides
//! the agent, like a sandbox, says which agent it runs with
//! [`AGENT_HINT`] in its environment.
//!
//! [`foreground`] lists the processes in front for `crystal process-info`:
//! each one's command and the directory it works in.

use crate::agent_rules;
use crate::catalog;
use crate::protocol::{Front, ProcessInfo};
use std::path::{Path, PathBuf};

/// The variable a program in front sets to say it runs that agent, for a
/// wrapper that hides the agent's own process: `CRYSTAL_AGENT=claude fence
/// -- claude`. Only the process in front is looked at, so it's for the
/// wrapper's command, not for exporting.
pub const AGENT_HINT: &str = "CRYSTAL_AGENT";

/// The shells crystal recognises, by program name.
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "elvish", "xonsh",
];

/// Programs that run a script named in their arguments, rather than being
/// what runs: the script says what's in front.
const INTERPRETERS: &[&str] = &[
    "node", "nodejs", "bun", "deno", "python", "python3", "ruby", "perl",
];

/// What's in front in the terminal whose foreground process group is led
/// by `leader`, or `None` when the process can't be read (it just ended,
/// or this system doesn't say).
pub fn of_process(leader: i32) -> Option<Front> {
    let process = read_process(leader)?;
    let hinted = process.hint.as_deref().and_then(agent_named);
    Some(hinted.unwrap_or_else(|| classify(&process.exe, &process.args)))
}

/// The processes in the foreground process group `group`, the job a
/// terminal's keys go to: its leader first, then the rest by their pids.
/// Those that can't be read, say ended since, are left out.
pub fn foreground(group: i32) -> Vec<ProcessInfo> {
    let mut pids = group_members(group);
    pids.sort_by_key(|&pid| (pid != group, pid));
    pids.dedup();
    pids.into_iter()
        .filter_map(|pid| {
            let process = read_process(pid)?;
            let name = process
                .args
                .first()
                .map(|first| program_name(first))
                .filter(|name| !name.is_empty())
                .or_else(|| {
                    let name = process.exe.file_name()?;
                    Some(name.to_string_lossy().into_owned())
                })
                .unwrap_or_default();
            Some(ProcessInfo {
                pid,
                name,
                argv: process.args,
                cwd: working_dir(pid),
            })
        })
        .collect()
}

/// What a session's own command is, as if it were in front: until the
/// daemon has looked at what really is, this is the best guess there is.
pub fn of_command(command: &[String]) -> Option<Front> {
    let program = command.first()?;
    Some(classify(Path::new(program), command))
}

/// What a program is, from its executable's path and its arguments, the
/// first of them its name as it was run.
pub fn classify(exe: &Path, args: &[String]) -> Front {
    let run_as = args
        .first()
        .map(|first| program_name(first))
        .unwrap_or_default();
    let exe_name = exe
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    if let Some(front) = agent_named(&run_as).or_else(|| agent_named(&exe_name)) {
        return front;
    }
    // Claude Code's own binary is named after its version.
    if exe.to_string_lossy().contains("/claude/versions/") {
        return agent("claude");
    }
    let is_shell = SHELLS.contains(&run_as.as_str());
    let runs_scripts = is_shell || INTERPRETERS.contains(&run_as.as_str());
    if runs_scripts && let Some(script) = script(args) {
        return of_script(script);
    }
    if is_shell {
        return Front::Shell { name: run_as };
    }
    let name = if run_as.is_empty() { exe_name } else { run_as };
    Front::Program { name }
}

/// The script an interpreter or a shell was given to run: its first
/// argument that isn't an option. A shell given `-c` runs a command line,
/// not a script, so it has none.
fn script(args: &[String]) -> Option<&str> {
    let mut rest = args.iter().skip(1);
    for arg in rest.by_ref() {
        if arg == "-c" || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c')) {
            return None;
        }
        if !arg.starts_with('-') {
            return Some(arg);
        }
    }
    None
}

/// What runs as `script`: an agent named for it, or one of the npm
/// packages an agent's rules name, or else a program named after the
/// script.
fn of_script(script: &str) -> Front {
    if let Some(rules) = agent_rules::current().by_script(script) {
        return agent(&rules.id);
    }
    let name = program_name(script);
    if let Some(front) = agent_named(&name) {
        return front;
    }
    // A package's own script runs as the package: `vite`, not `vite.js`.
    let name = package_of(script).unwrap_or(name);
    Front::Program { name }
}

/// The npm package `script` belongs to, by its last `node_modules/`, its
/// scope dropped: `vite` for `…/node_modules/vite/bin/vite.js`.
fn package_of(script: &str) -> Option<String> {
    let (_, after) = script.rsplit_once("node_modules/")?;
    let mut parts = after.split('/');
    let first = parts.next()?;
    let package = if first.starts_with('@') {
        parts.next()?
    } else {
        first
    };
    Some(package.to_string())
}

/// The agent run as `name`, if crystal knows one by that name: one the
/// new-session panel offers, or one there are rules to read the screen of.
fn agent_named(name: &str) -> Option<Front> {
    if let Some(known) = catalog::find(name) {
        return Some(Front::Agent {
            program: known.program.to_string(),
            name: known.name.to_string(),
        });
    }
    let registry = agent_rules::current();
    let rules = registry.find(name)?;
    // An agent run by another of its names is in the catalog by that one.
    let known = std::iter::once(&rules.id)
        .chain(&rules.aliases)
        .find_map(|name| catalog::find(name));
    Some(match known {
        Some(known) => Front::Agent {
            program: known.program.to_string(),
            name: known.name.to_string(),
        },
        None => Front::Agent {
            program: name.to_string(),
            name: rules.name.clone(),
        },
    })
}

fn agent(program: &str) -> Front {
    agent_named(program).unwrap_or(Front::Program {
        name: program.to_string(),
    })
}

/// A program's name as a path's last part, without the `-` a login shell
/// is started with.
fn program_name(path: &str) -> String {
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.trim_start_matches('-').to_string()
}

/// A running process, as much of it as tells what it is.
struct Process {
    exe: PathBuf,
    args: Vec<String>,
    /// The agent its environment says it runs, by [`AGENT_HINT`].
    hint: Option<String>,
}

/// The agent an environment's strings, `NAME=value` each, say a process
/// runs: a value of [`AGENT_HINT`] that isn't empty.
fn hint<'a>(env: impl IntoIterator<Item = &'a str>) -> Option<String> {
    env.into_iter()
        .filter_map(|var| var.strip_prefix(AGENT_HINT)?.strip_prefix('='))
        .map(str::trim)
        .find(|agent| !agent.is_empty())
        .map(String::from)
}

#[cfg(target_os = "linux")]
fn read_process(pid: i32) -> Option<Process> {
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap_or_default();
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args = cmdline
        .split(|&byte| byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    // Another user's process keeps its environment to itself.
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    let env = environ
        .split(|&byte| byte == 0)
        .map(|var| String::from_utf8_lossy(var).into_owned())
        .collect::<Vec<String>>();
    let hint = hint(env.iter().map(String::as_str));
    Some(Process { exe, args, hint })
}

#[cfg(target_os = "macos")]
fn read_process(pid: i32) -> Option<Process> {
    let (args, env) = macos::arguments(pid)?;
    Some(Process {
        exe: macos::executable(pid).unwrap_or_default(),
        args,
        hint: hint(env.iter().map(String::as_str)),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_process(_pid: i32) -> Option<Process> {
    None
}

/// The pids of the processes in the process group `group`.
#[cfg(target_os = "linux")]
fn group_members(group: i32) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let group_of = |pid: i32| -> Option<i32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // After the name in brackets, which may hold anything: the state,
        // the parent, then the process group.
        let (_, after) = stat.rsplit_once(')')?;
        after.split_whitespace().nth(2)?.parse().ok()
    };
    dir.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| group_of(pid) == Some(group))
        .collect()
}

#[cfg(target_os = "macos")]
fn group_members(group: i32) -> Vec<i32> {
    macos::group_members(group)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn group_members(group: i32) -> Vec<i32> {
    vec![group]
}

/// The directory process `pid` works in, when the system says.
#[cfg(target_os = "linux")]
pub fn working_dir(pid: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(target_os = "macos")]
pub fn working_dir(pid: i32) -> Option<PathBuf> {
    macos::working_dir(pid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn working_dir(_pid: i32) -> Option<PathBuf> {
    None
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    /// The path of the executable process `pid` runs.
    pub fn executable(pid: i32) -> Option<PathBuf> {
        let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: the buffer is as long as the size given, which is the
        // most proc_pidpath writes.
        let written =
            unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        if written <= 0 {
            return None;
        }
        buffer.truncate(written as usize);
        Some(PathBuf::from(String::from_utf8_lossy(&buffer).into_owned()))
    }

    /// The pids of the processes in the process group `group`.
    pub fn group_members(group: i32) -> Vec<i32> {
        let mut pids = vec![0 as libc::pid_t; 1024];
        let room = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
        // SAFETY: the buffer is as long as the size given, and
        // proc_listpgrppids writes at most that, giving back how many pids.
        let count = unsafe { libc::proc_listpgrppids(group, pids.as_mut_ptr().cast(), room) };
        if count <= 0 {
            return Vec::new();
        }
        pids.truncate(count as usize);
        pids.retain(|&pid| pid > 0);
        pids
    }

    /// The directory process `pid` works in.
    pub fn working_dir(pid: i32) -> Option<PathBuf> {
        // SAFETY: an all-zero proc_vnodepathinfo is a valid value for
        // proc_pidinfo to fill in, and the size given is its own.
        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&mut info as *mut libc::proc_vnodepathinfo).cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        let path: Vec<u8> = info
            .pvi_cdir
            .vip_path
            .iter()
            .flatten()
            .map(|&byte| byte as u8)
            .take_while(|&byte| byte != 0)
            .collect();
        (!path.is_empty()).then(|| PathBuf::from(String::from_utf8_lossy(&path).into_owned()))
    }

    /// The arguments process `pid` was started with, and its environment,
    /// from the kernel's copy: the count of the arguments, the executable's
    /// path, then each argument, then each `NAME=value`, all ended by NULs.
    pub fn arguments(pid: i32) -> Option<(Vec<String>, Vec<String>)> {
        let mut buffer = vec![0u8; argument_space()];
        let mut size = buffer.len();
        let mut name = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
        // SAFETY: `name` and `buffer` are valid for the lengths given, and
        // sysctl writes at most `size` bytes, then sets `size` to how many.
        let failed = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                name.len() as u32,
                buffer.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if failed != 0 {
            return None;
        }
        buffer.truncate(size);
        Some(parse_arguments(&buffer))
    }

    /// The most room a process's arguments take, which the kernel says once.
    fn argument_space() -> usize {
        static SPACE: OnceLock<usize> = OnceLock::new();
        *SPACE.get_or_init(|| {
            let mut space: libc::c_int = 0;
            let mut size = std::mem::size_of::<libc::c_int>();
            let mut name = [libc::CTL_KERN, libc::KERN_ARGMAX];
            // SAFETY: `space` is a c_int and `size` says so.
            let failed = unsafe {
                libc::sysctl(
                    name.as_mut_ptr(),
                    name.len() as u32,
                    (&mut space as *mut libc::c_int).cast(),
                    &mut size,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if failed == 0 && space > 0 {
                space as usize
            } else {
                256 * 1024
            }
        })
    }

    /// The arguments and the environment in what KERN_PROCARGS2 gives: a
    /// count, then the executable's path and NUL padding, then the
    /// arguments, then the environment, up to an empty string.
    pub fn parse_arguments(buffer: &[u8]) -> (Vec<String>, Vec<String>) {
        let Some(count) = buffer.get(..4) else {
            return (Vec::new(), Vec::new());
        };
        let count = i32::from_ne_bytes([count[0], count[1], count[2], count[3]]).max(0) as usize;
        let rest = &buffer[4..];
        // Past the executable's path, then the NULs after it.
        let Some(path_end) = rest.iter().position(|&byte| byte == 0) else {
            return (Vec::new(), Vec::new());
        };
        let Some(start) = rest[path_end..].iter().position(|&byte| byte != 0) else {
            return (Vec::new(), Vec::new());
        };
        let mut strings = rest[path_end + start..]
            .split(|&byte| byte == 0)
            .map(|string| String::from_utf8_lossy(string).into_owned());
        let args = strings.by_ref().take(count).collect();
        let env = strings.take_while(|var| !var.is_empty()).collect();
        (args, env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn front(exe: &str, args: &[&str]) -> Front {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        classify(&PathBuf::from(exe), &args)
    }

    fn agent(program: &str, name: &str) -> Front {
        Front::Agent {
            program: program.into(),
            name: name.into(),
        }
    }

    #[test]
    fn an_agent_is_known_by_the_name_it_was_run_as() {
        let claude = agent("claude", "Claude Code");
        assert_eq!(front("/usr/local/bin/claude", &["claude"]), claude);
        let codex = front(
            "/opt/codex/releases/0.160.0/bin/codex",
            &["codex", "-m", "x"],
        );
        assert_eq!(codex, agent("codex", "Codex"));
    }

    #[test]
    fn claude_code_s_own_binary_is_known_by_its_path() {
        let exe = "/Users/me/.local/share/claude/versions/2.1.288";
        assert_eq!(
            front(exe, &["/Users/me/.local/bin/claude", "--resume"]),
            agent("claude", "Claude Code")
        );
        assert_eq!(front(exe, &["2.1.288"]), agent("claude", "Claude Code"));
    }

    #[test]
    fn an_agent_run_by_node_is_known_by_its_package() {
        let script = "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js";
        assert_eq!(
            front("/usr/bin/node", &["node", "--no-warnings", script]),
            agent("claude", "Claude Code")
        );
    }

    #[test]
    fn an_agent_crystal_only_has_rules_for_is_an_agent_too() {
        // Maki isn't in the new-session panel, but its screen can be read.
        assert_eq!(front("/usr/bin/maki", &["maki"]), agent("maki", "Maki"));
        // Another name of an agent in the panel is that agent.
        assert_eq!(
            front("/usr/bin/cursor", &["cursor"]),
            agent("cursor-agent", "Cursor")
        );
        let qwen = "/usr/lib/node_modules/@qwen-code/qwen-code/dist/index.js";
        assert_eq!(
            front("/usr/bin/node", &["node", qwen]),
            agent("qwen", "Qwen Code")
        );
    }

    #[test]
    fn an_agent_that_is_a_script_is_known_by_the_script() {
        let fake = front(
            "/bin/bash",
            &["/bin/sh", "/tmp/bin/claude", "--settings", "{}"],
        );
        assert_eq!(fake, agent("claude", "Claude Code"));
    }

    #[test]
    fn a_shell_is_a_shell_even_running_a_command_line() {
        let zsh = Front::Shell { name: "zsh".into() };
        assert_eq!(front("/bin/zsh", &["-zsh"]), zsh);
        assert_eq!(front("/bin/zsh", &["zsh", "-l"]), zsh);
        let sh = front("/bin/sh", &["sh", "-c", "echo hi; sleep 30"]);
        assert_eq!(sh, Front::Shell { name: "sh".into() });
    }

    #[test]
    fn other_programs_are_known_by_their_name() {
        let sleep = front("/bin/sleep", &["sleep", "30"]);
        assert_eq!(
            sleep,
            Front::Program {
                name: "sleep".into()
            }
        );
        let vite = "/code/app/node_modules/vite/bin/vite.js";
        assert_eq!(
            front("/usr/bin/node", &["node", vite]),
            Front::Program {
                name: "vite".into()
            }
        );
        let script = front("/bin/bash", &["bash", "build.sh"]);
        assert_eq!(
            script,
            Front::Program {
                name: "build.sh".into()
            }
        );
    }

    #[test]
    fn a_process_without_arguments_goes_by_its_executable() {
        assert_eq!(
            front("/usr/bin/top", &[]),
            Front::Program { name: "top".into() }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_arguments_skip_the_path_and_its_padding() {
        let mut buffer = 2i32.to_ne_bytes().to_vec();
        buffer
            .extend_from_slice(b"/bin/zsh\0\0\0\0zsh\0-l\0HOME=/x\0CRYSTAL_AGENT=claude\0\0junk\0");
        let (args, env) = macos::parse_arguments(&buffer);
        assert_eq!(args, ["zsh", "-l"]);
        assert_eq!(env, ["HOME=/x", "CRYSTAL_AGENT=claude"]);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn this_process_reads_as_itself() {
        let me = read_process(std::process::id() as i32).unwrap();
        assert!(!me.args.is_empty());
        assert!(me.exe.is_absolute());
        let group = unsafe { libc::getpgrp() };
        let pids: Vec<i32> = foreground(group)
            .iter()
            .map(|process| process.pid)
            .collect();
        assert!(pids.contains(&(std::process::id() as i32)), "{pids:?}");
        let mine = foreground(group)
            .into_iter()
            .find(|process| process.pid == std::process::id() as i32)
            .unwrap();
        assert_eq!(mine.cwd, std::env::current_dir().ok());
    }

    #[test]
    fn a_wrapper_says_which_agent_it_runs() {
        assert_eq!(
            hint(["HOME=/x", "CRYSTAL_AGENT=claude"]),
            Some("claude".into())
        );
        assert_eq!(hint(["CRYSTAL_AGENT=", "PATH=/bin"]), None);
        assert_eq!(hint(["CRYSTAL_AGENT_HOOKS=1"]), None);
        assert_eq!(hint(Vec::<&str>::new()), None);
    }
}
