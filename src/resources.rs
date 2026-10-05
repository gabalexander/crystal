//! The memory crystal's processes take, for the TUI's RAM view and its
//! footer: one look at every process on the machine (`/proc` on Linux,
//! `ps` elsewhere), then each session's program summed with every process
//! under it, since an agent runs node workers, shells and MCP servers and
//! the whole tree is what it costs; the daemon's own process and the
//! client's that asked; and how much memory the machine has. Adapted from
//! docket's metrics.
//!
//! What's counted is each process's resident set: the memory it has in
//! RAM now, shared pages counted in each process that maps them.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// What a process, and with a session's, every process under it, takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "ProcessUsage"))]
pub struct Usage {
    pub pid: u32,
    /// Resident memory, in bytes.
    pub bytes: u64,
    /// How many processes that is: none once the process has gone.
    pub processes: u32,
}

/// What a session's processes take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SessionUsage {
    pub name: String,
    pub usage: Usage,
}

/// One look at what crystal takes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Resources {
    /// The daemon's own process, without its sessions'.
    pub daemon: Usage,
    /// The client that asked, when it said which process it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Usage>,
    /// Each session whose program runs, in the daemon's order.
    pub sessions: Vec<SessionUsage>,
    /// The memory the machine has, in bytes: 0 when it can't say.
    pub total: u64,
}

impl Resources {
    /// What crystal takes itself: the daemon and the client that asked.
    pub fn own(&self) -> u64 {
        self.daemon.bytes + self.client.map_or(0, |client| client.bytes)
    }

    /// Everything: crystal itself and every session.
    pub fn all(&self) -> u64 {
        self.own() + self.sessions.iter().map(|s| s.usage.bytes).sum::<u64>()
    }
}

/// A process, as the look at them all found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Process {
    pid: u32,
    parent: u32,
    bytes: u64,
}

/// Looks at what the daemon, whose process is `daemon`, the client whose
/// process is `client`, and each of `sessions`, a name and the process of
/// its program, take now. It reads every process on the machine, or runs
/// `ps`: not for an event loop.
pub fn measure(daemon: u32, client: Option<u32>, sessions: &[(String, u32)]) -> Resources {
    of(&processes(), daemon, client, sessions, machine_bytes())
}

fn of(
    processes: &[Process],
    daemon: u32,
    client: Option<u32>,
    sessions: &[(String, u32)],
    total: u64,
) -> Resources {
    let bytes: HashMap<u32, u64> = processes.iter().map(|p| (p.pid, p.bytes)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in processes {
        children
            .entry(process.parent)
            .or_default()
            .push(process.pid);
    }
    let alone = |pid: u32| Usage {
        pid,
        bytes: bytes.get(&pid).copied().unwrap_or(0),
        processes: u32::from(bytes.contains_key(&pid)),
    };
    let sessions = sessions
        .iter()
        .map(|(name, pid)| SessionUsage {
            name: name.clone(),
            usage: tree(*pid, &bytes, &children),
        })
        .collect();
    Resources {
        daemon: alone(daemon),
        client: client.map(alone),
        sessions,
        total,
    }
}

/// What the process `root` and every process under it take. A process
/// seen twice, a pid reused while the processes were being read, counts
/// once; a root that has gone takes nothing.
fn tree(root: u32, bytes: &HashMap<u32, u64>, children: &HashMap<u32, Vec<u32>>) -> Usage {
    let mut usage = Usage {
        pid: root,
        ..Usage::default()
    };
    let mut seen = HashSet::new();
    let mut left = vec![root];
    while let Some(pid) = left.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(taken) = bytes.get(&pid) {
            usage.bytes += taken;
            usage.processes += 1;
        }
        if let Some(under) = children.get(&pid) {
            left.extend(under);
        }
    }
    usage
}

/// Every process on the machine, from `/proc`.
#[cfg(target_os = "linux")]
fn processes() -> Vec<Process> {
    let page = page_bytes();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            let statm = std::fs::read_to_string(entry.path().join("statm")).ok()?;
            let parent = parent_in_stat(&stat)?;
            let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
            Some(Process {
                pid,
                parent,
                bytes: pages * page,
            })
        })
        .collect()
}

/// The parent's pid in a `/proc/<pid>/stat` line: the field after the
/// state, past the program's name in brackets, which can hold anything.
#[cfg(any(target_os = "linux", test))]
fn parent_in_stat(stat: &str) -> Option<u32> {
    let (_, after) = stat.rsplit_once(')')?;
    after.split_whitespace().nth(1)?.parse().ok()
}

/// Every process on the machine, from `ps`.
#[cfg(not(target_os = "linux"))]
fn processes() -> Vec<Process> {
    let listed = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,rss="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match listed {
        Ok(output) => from_ps(&String::from_utf8_lossy(&output.stdout)),
        Err(_) => Vec::new(),
    }
}

/// What `ps -axo pid=,ppid=,rss=` printed, a process a line, its resident
/// memory in kilobytes. A line that doesn't read is passed over.
#[cfg(any(not(target_os = "linux"), test))]
fn from_ps(listed: &str) -> Vec<Process> {
    listed
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace().map(str::parse::<u64>);
            let (Some(Ok(pid)), Some(Ok(parent)), Some(Ok(kilobytes))) =
                (fields.next(), fields.next(), fields.next())
            else {
                return None;
            };
            Some(Process {
                pid: u32::try_from(pid).ok()?,
                parent: u32::try_from(parent).ok()?,
                bytes: kilobytes * 1024,
            })
        })
        .collect()
}

/// How big a page of memory is, in bytes.
fn page_bytes() -> u64 {
    // SAFETY: sysconf only reads a value of the system's.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(page).unwrap_or(4096)
}

/// The memory the machine has, in bytes, or 0 when it doesn't say.
fn machine_bytes() -> u64 {
    // SAFETY: sysconf only reads a value of the system's.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    u64::try_from(pages).map_or(0, |pages| pages * page_bytes())
}

/// An amount of memory as a person reads it: `512 KB`, `412 MB`, `1.2 GB`.
pub fn size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    match bytes {
        0..MB => format!("{} KB", bytes.div_ceil(KB)),
        MB..GB => format!("{} MB", (bytes + MB / 2) / MB),
        _ => format!("{:.1} GB", bytes as f64 / GB as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, pid: u32) -> (String, u32) {
        (name.to_string(), pid)
    }

    #[test]
    fn a_session_takes_what_its_whole_tree_of_processes_takes() {
        // The daemon is 10, its sessions' programs 20 and 30; 20 runs 21
        // and 22, and 22 runs 23. 99 is nobody's, and 40 is the client.
        let listed = " 10     1  1000\n 20    10  2000\n 21    20   300\n 22    20   500\n \
                      23    22   200\n 30    10    50\n 99     1  9999\n 40     7   400\n";
        let taken = of(
            &from_ps(listed),
            10,
            Some(40),
            &[named("agent", 20), named("shell", 30)],
            8 << 30,
        );
        assert_eq!(taken.daemon.bytes, 1000 * 1024);
        assert_eq!(taken.client.unwrap().bytes, 400 * 1024);
        let agent = taken.sessions[0].usage;
        assert_eq!((agent.bytes, agent.processes), ((3000) * 1024, 4));
        let shell = taken.sessions[1].usage;
        assert_eq!((shell.bytes, shell.processes), (50 * 1024, 1));
        assert_eq!(taken.own(), 1400 * 1024);
        assert_eq!(taken.all(), (1400 + 3050) * 1024);
        assert_eq!(taken.total, 8 << 30);
    }

    #[test]
    fn a_process_that_has_gone_takes_nothing_and_a_cycle_counts_once() {
        let listed = " 20 21 100\n 21 20 50\nnot a process\n";
        let taken = of(
            &from_ps(listed),
            10,
            None,
            &[named("a", 20), named("b", 5)],
            0,
        );
        assert_eq!(taken.sessions[0].usage.bytes, 150 * 1024);
        assert_eq!(
            taken.sessions[1].usage,
            Usage {
                pid: 5,
                ..Usage::default()
            }
        );
        assert_eq!(taken.daemon.processes, 0);
    }

    #[test]
    fn the_parent_is_read_past_a_name_with_brackets_and_spaces() {
        let stat = "123 (my (odd) prog) S 45 123 123 0 -1 4194560 100";
        assert_eq!(parent_in_stat(stat), Some(45));
        assert_eq!(parent_in_stat("garbage"), None);
    }

    #[test]
    fn sizes_read_as_a_person_says_them() {
        assert_eq!(size(500), "1 KB");
        assert_eq!(size(412 * 1024 * 1024 + 1), "412 MB");
        assert_eq!(size(1288 * 1024 * 1024), "1.3 GB");
    }

    #[test]
    fn the_machine_has_memory_and_crystal_takes_some_of_it() {
        assert!(machine_bytes() > 0);
        let me = std::process::id();
        let taken = measure(me, None, &[named("me", me)]);
        assert!(taken.daemon.bytes > 0);
        assert!(taken.sessions[0].usage.processes >= 1);
    }
}
