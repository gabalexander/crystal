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
//! as its script. So [`classify`] looks at all of them.

use crate::catalog;
use crate::protocol::Front;
use std::path::Path;

/// The shells crystal recognises, by program name.
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "elvish", "xonsh",
];

/// Programs that run a script named in their arguments, rather than being
/// what runs: the script says what's in front.
const INTERPRETERS: &[&str] = &[
    "node", "nodejs", "bun", "deno", "python", "python3", "ruby", "perl",
];

/// npm packages of agents, which run as `node <package>/…`: the package,
/// and the program it stands for in the catalog.
const AGENT_PACKAGES: &[(&str, &str)] = &[
    ("@anthropic-ai/claude-code", "claude"),
    ("@openai/codex", "codex"),
    ("@google/gemini-cli", "gemini"),
    ("opencode-ai", "opencode"),
];

/// What's in front in the terminal whose foreground process group is led
/// by `leader`, or `None` when the process can't be read (it just ended,
/// or this system doesn't say).
pub fn of_process(leader: i32) -> Option<Front> {
    let process = read_process(leader)?;
    Some(classify(&process.exe, &process.args))
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

/// What runs as `script`: an agent named for it, or an agent's npm
/// package, or else a program named after the script.
fn of_script(script: &str) -> Front {
    for (package, program) in AGENT_PACKAGES {
        if script.contains(&format!("node_modules/{package}/")) {
            return agent(program);
        }
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

/// The agent run as `name`, if crystal knows one by that name.
fn agent_named(name: &str) -> Option<Front> {
    catalog::find(name).map(|known| Front::Agent {
        program: known.program.to_string(),
        name: known.name.to_string(),
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
    exe: std::path::PathBuf,
    args: Vec<String>,
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
    Some(Process { exe, args })
}

#[cfg(target_os = "macos")]
fn read_process(pid: i32) -> Option<Process> {
    Some(Process {
        exe: macos::executable(pid).unwrap_or_default(),
        args: macos::arguments(pid)?,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_process(_pid: i32) -> Option<Process> {
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

    /// The arguments process `pid` was started with, from the kernel's
    /// copy: the count of them, the executable's path, and then each
    /// argument, all ended by NULs.
    pub fn arguments(pid: i32) -> Option<Vec<String>> {
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

    /// The arguments in what KERN_PROCARGS2 gives: a count, then the
    /// executable's path and NUL padding, then the arguments.
    pub fn parse_arguments(buffer: &[u8]) -> Vec<String> {
        let Some(count) = buffer.get(..4) else {
            return Vec::new();
        };
        let count = i32::from_ne_bytes([count[0], count[1], count[2], count[3]]);
        let rest = &buffer[4..];
        // Past the executable's path, then the NULs after it.
        let Some(path_end) = rest.iter().position(|&byte| byte == 0) else {
            return Vec::new();
        };
        let Some(start) = rest[path_end..].iter().position(|&byte| byte != 0) else {
            return Vec::new();
        };
        rest[path_end + start..]
            .split(|&byte| byte == 0)
            .take(count.max(0) as usize)
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect()
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
        buffer.extend_from_slice(b"/bin/zsh\0\0\0\0zsh\0-l\0HOME=/x\0");
        assert_eq!(macos::parse_arguments(&buffer), ["zsh", "-l"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_process_reads_as_itself() {
        let me = read_process(std::process::id() as i32).unwrap();
        assert!(!me.args.is_empty());
        assert!(me.exe.is_absolute());
    }
}
