//! What crystal's processes take, memory and CPU, for the TUI's resources
//! view, its footer and `crystal usage`: one look at every process on the
//! machine (`/proc` on Linux, libproc on a Mac), then each session's
//! program summed with every process under it, since an agent runs node
//! workers, shells and MCP servers and the whole tree is what it costs; the
//! daemon's own process, and apart from it each process it runs that isn't
//! a session's, its helpers (the distiller's `claude -p`, git, a plugin's
//! hook), with every process under each; the client's that asked, with
//! those under it; the agent kept warm; and what the machine has. Adapted
//! from docket's metrics.
//!
//! A process's memory is what it has in RAM now. On a Mac that's its
//! physical footprint, what Activity Monitor's Memory column shows, which
//! counts what it has on the GPU, as memory's models on Metal do in the
//! daemon; on Linux it's its resident set, shared pages counted in each
//! process that maps them.
//!
//! Its CPU is a rate: the CPU time it had, user and system, between an
//! earlier look and this one, over the time between them, in percent of
//! one core, 100 being one core busy all that while. [`Earlier`] keeps the
//! time each process had, by its pid and when it started, so that a pid
//! given to another process meanwhile isn't taken for the same one; a
//! process the earlier look didn't see started since, so all its CPU time
//! counts.
//!
//! And what runs under a session's program, for whether it may be stopped
//! as it sits idle: anything at all, and a job cut loose from its terminal.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// What a process, and with a session's, every process under it, takes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "ProcessUsage"))]
pub struct Usage {
    pub pid: u32,
    /// Memory, in bytes: the physical footprint on a Mac, the resident set
    /// on Linux.
    pub bytes: u64,
    /// CPU since the look counted from, in percent of one core, to a tenth:
    /// 100 is one core busy all that while.
    pub cpu: f64,
    /// How many processes that is: none once the process has gone.
    pub processes: u32,
}

/// What a session's processes take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SessionUsage {
    pub name: String,
    pub usage: Usage,
}

/// What a process the daemon runs that isn't a session's takes, with every
/// process under it: the distiller's `claude -p`, git, a plugin's hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[cfg_attr(test, schemars(rename = "HelperUsage"))]
pub struct Helper {
    /// Its program's name, as the system keeps it: cut to 15 bytes.
    pub name: String,
    pub usage: Usage,
}

/// What the machine has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Machine {
    /// Its memory, in bytes: 0 when it can't say.
    pub bytes: u64,
    /// Its cores, those online: 0 when it can't say.
    pub cores: u32,
}

/// One look at what crystal takes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Resources {
    /// The daemon's own process, without its sessions' or its helpers'.
    pub daemon: Usage,
    /// The processes the daemon runs that aren't sessions' programs or the
    /// agent kept warm, each with every process under it.
    #[serde(default)]
    pub helpers: Vec<Helper>,
    /// The client that asked, with every process under it, when it said
    /// which process it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Usage>,
    /// Each session whose program runs, in the daemon's order.
    pub sessions: Vec<SessionUsage>,
    /// The agent kept warm for a new session to take over, while there's
    /// one: `[sessions] warm_agent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warm: Option<Usage>,
    pub machine: Machine,
    /// How long CPU was counted over, in milliseconds: 0 when it couldn't
    /// be, every process's CPU then 0.
    pub cpu_over_ms: u64,
}

/// What several processes take together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Total {
    pub bytes: u64,
    pub cpu: f64,
    pub processes: u32,
}

impl Total {
    fn of<'a>(usages: impl IntoIterator<Item = &'a Usage>) -> Total {
        let mut total = Total::default();
        for usage in usages {
            total.bytes += usage.bytes;
            total.cpu += usage.cpu;
            total.processes += usage.processes;
        }
        total.cpu = tenths(total.cpu);
        total
    }
}

impl From<&Usage> for Total {
    fn from(usage: &Usage) -> Total {
        Total::of([usage])
    }
}

impl Resources {
    /// What crystal takes itself: the daemon, its helpers and the client
    /// that asked.
    pub fn own(&self) -> Total {
        Total::of(self.crystal())
    }

    /// Everything: crystal itself, every session and the agent kept warm.
    pub fn all(&self) -> Total {
        let sessions = self.sessions.iter().map(|session| &session.usage);
        Total::of(self.crystal().chain(&self.warm).chain(sessions))
    }

    fn crystal(&self) -> impl Iterator<Item = &Usage> {
        let helpers = self.helpers.iter().map(|helper| &helper.usage);
        std::iter::once(&self.daemon)
            .chain(helpers)
            .chain(&self.client)
    }
}

/// A process, as the look at them all found it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Process {
    pid: u32,
    parent: u32,
    /// When it started, in the system's own units: only to tell it from a
    /// process given its pid later.
    start: u64,
    bytes: u64,
    /// The CPU time it has had, user and system, in nanoseconds.
    cpu: u64,
    /// Its program's name, as the system keeps it.
    name: String,
}

/// How long a look counts CPU over at least: two clients asking a moment
/// apart would count over that moment, which says little, a process's CPU
/// time going up in ticks of 10 ms on Linux.
const WINDOW: Duration = Duration::from_millis(500);

/// How long ago a look may be to count from: CPU counted over longer says
/// what the processes took then rather than now. The TUI looks every five
/// seconds.
const STALE: Duration = Duration::from_secs(10);

/// The CPU time each process had at the looks before, for the next to count
/// from: the newest at least [`WINDOW`] before it, and none older than
/// [`STALE`]. The daemon keeps one, which every client's look counts from.
#[derive(Debug, Default)]
pub struct Earlier {
    older: Option<Mark>,
    newer: Option<Mark>,
}

/// A look's CPU times.
#[derive(Debug)]
struct Mark {
    at: Instant,
    /// Each process's start and CPU time, by its pid.
    times: HashMap<u32, (u64, u64)>,
}

/// The CPU time each process had since a look counted from, in
/// nanoseconds by its pid, and how long ago that look was.
#[derive(Debug)]
struct Spent {
    over: Duration,
    by: HashMap<u32, u64>,
}

impl Earlier {
    /// What each of `processes`, looked at `at`, spent since the look to
    /// count from, keeping this look to count from later; `None` with none
    /// to count from, the first look or the first in a while, which the
    /// next counts from.
    fn count(&mut self, at: Instant, processes: &[Process]) -> Option<Spent> {
        let fresh = |mark: &&Mark| at.saturating_duration_since(mark.at) <= STALE;
        let Some(newer) = self.newer.as_ref().filter(fresh) else {
            self.older = None;
            self.newer = Some(Mark::of(at, processes));
            return None;
        };
        let ripe = at.saturating_duration_since(newer.at) >= WINDOW;
        let from = match ripe {
            true => Some(newer),
            // A moment after the newer: from the one before it, which is a
            // window before the newer at least.
            false => self.older.as_ref().filter(fresh),
        };
        let spent = from.map(|from| from.spent(at, processes));
        if ripe {
            self.older = self.newer.replace(Mark::of(at, processes));
        }
        spent
    }
}

impl Mark {
    fn of(at: Instant, processes: &[Process]) -> Mark {
        let times = processes.iter();
        let times = times.map(|process| (process.pid, (process.start, process.cpu)));
        Mark {
            at,
            times: times.collect(),
        }
    }

    /// What each of `processes`, looked at `at`, spent since this look. One
    /// it didn't see, or saw by its pid but started at another time, has
    /// started since, so all its time counts; so does one whose time went
    /// back.
    fn spent(&self, at: Instant, processes: &[Process]) -> Spent {
        let by = processes.iter().map(|process| {
            let before = match self.times.get(&process.pid) {
                Some(&(start, cpu)) if start == process.start && cpu <= process.cpu => cpu,
                _ => 0,
            };
            (process.pid, process.cpu - before)
        });
        Spent {
            over: at.saturating_duration_since(self.at),
            by: by.collect(),
        }
    }
}

/// Whose processes a look is about.
pub struct Whose<'a> {
    pub daemon: u32,
    /// The client that asked.
    pub client: Option<u32>,
    /// Each session's name and its program's process.
    pub sessions: &'a [(String, u32)],
    /// The agent kept warm.
    pub warm: Option<u32>,
}

/// Looks at what `whose` processes take now, their CPU counted from a look
/// `earlier` keeps; with none to count from, it looks again a moment later
/// and counts from this one. It reads every process on the machine, and
/// may wait half a second: not for an event loop.
pub fn measure(earlier: &Mutex<Earlier>, whose: &Whose) -> Resources {
    let mut looked = processes();
    let mut spent = earlier.lock().unwrap().count(Instant::now(), &looked);
    if spent.is_none() {
        thread::sleep(WINDOW);
        looked = processes();
        spent = earlier.lock().unwrap().count(Instant::now(), &looked);
    }
    of(&looked, spent.as_ref(), whose, machine())
}

fn of(processes: &[Process], spent: Option<&Spent>, whose: &Whose, machine: Machine) -> Resources {
    let family = Family::of(processes, spent);
    let none = HashSet::new();
    let sessions = whose.sessions.iter().map(|(name, pid)| SessionUsage {
        name: name.clone(),
        usage: family.tree(*pid, &none),
    });
    // What the daemon runs that's neither a session's nor the agent kept
    // warm is its helpers', and what those run, though not a session's
    // should one be found under them.
    let theirs: HashSet<u32> = (whose.sessions.iter().map(|(_, pid)| *pid))
        .chain(whose.warm)
        .collect();
    let mut helpers: Vec<&Process> = (family.children.get(&whose.daemon).into_iter().flatten())
        .filter(|pid| !theirs.contains(pid))
        .filter_map(|pid| Some(family.by_pid.get(pid)?.0))
        .collect();
    helpers.sort_by_key(|helper| helper.pid);
    let helpers = helpers.into_iter().map(|helper| Helper {
        name: helper.name.clone(),
        usage: family.tree(helper.pid, &theirs),
    });
    Resources {
        daemon: family.alone(whose.daemon),
        helpers: helpers.collect(),
        client: whose.client.map(|client| family.tree(client, &none)),
        sessions: sessions.collect(),
        warm: whose.warm.map(|warm| family.tree(warm, &none)),
        machine,
        cpu_over_ms: spent.map_or(0, |spent| {
            u64::try_from(spent.over.as_millis()).unwrap_or(u64::MAX)
        }),
    }
}

/// A look's processes by their pids, each with its CPU in percent of a
/// core, and the processes under each.
struct Family<'a> {
    by_pid: HashMap<u32, (&'a Process, f64)>,
    children: HashMap<u32, Vec<u32>>,
}

impl<'a> Family<'a> {
    fn of(processes: &'a [Process], spent: Option<&Spent>) -> Family<'a> {
        let percent = |pid: u32| match spent {
            Some(spent) if !spent.over.is_zero() => {
                let nanos = spent.by.get(&pid).copied().unwrap_or(0);
                nanos as f64 * 100.0 / spent.over.as_nanos() as f64
            }
            _ => 0.0,
        };
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for process in processes {
            let under = children.entry(process.parent).or_default();
            under.push(process.pid);
        }
        let by_pid = processes.iter();
        let by_pid = by_pid.map(|process| (process.pid, (process, percent(process.pid))));
        Family {
            by_pid: by_pid.collect(),
            children,
        }
    }

    /// What the process `pid` takes alone.
    fn alone(&self, pid: u32) -> Usage {
        let (bytes, cpu) = self
            .by_pid
            .get(&pid)
            .map_or((0, 0.0), |&(process, cpu)| (process.bytes, cpu));
        Usage {
            pid,
            bytes,
            cpu: tenths(cpu),
            processes: u32::from(self.by_pid.contains_key(&pid)),
        }
    }

    /// What the process `root` and every process under it take, but those
    /// in `skip` and under them. A process seen twice, a pid reused while
    /// the processes were being read, counts once; a root that has gone
    /// takes nothing.
    fn tree(&self, root: u32, skip: &HashSet<u32>) -> Usage {
        let mut usage = Usage {
            pid: root,
            ..Usage::default()
        };
        let mut seen = HashSet::new();
        let mut left = vec![root];
        while let Some(pid) = left.pop() {
            if !seen.insert(pid) || (pid != root && skip.contains(&pid)) {
                continue;
            }
            if let Some(&(process, cpu)) = self.by_pid.get(&pid) {
                usage.bytes += process.bytes;
                usage.cpu += cpu;
                usage.processes += 1;
            }
            if let Some(under) = self.children.get(&pid) {
                left.extend(under);
            }
        }
        usage.cpu = tenths(usage.cpu);
        usage
    }
}

/// A share of a core to a tenth of a percent, which is as near as anyone
/// reads it, and keeps JSON's numbers short.
fn tenths(percent: f64) -> f64 {
    (percent * 10.0).round() / 10.0
}

/// Every process on the machine, from `/proc`.
#[cfg(target_os = "linux")]
fn processes() -> Vec<Process> {
    // SAFETY: sysconf only reads a value of the system's.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let tick = 1_000_000_000
        / u64::try_from(ticks)
            .ok()
            .filter(|&ticks| ticks > 0)
            .unwrap_or(100);
    let page = page_bytes();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            process_in_stat(pid, &stat, page, tick)
        })
        .collect()
}

/// A process as its `/proc/<pid>/stat` line says: its program's name in
/// brackets, which can hold anything, then its state, its parent, its CPU
/// time in ticks of `tick` nanoseconds, user and system, when it started,
/// in ticks since the machine did, and its resident set in pages of `page`
/// bytes. Fields are counted as `man proc` counts them, from 1.
#[cfg(any(target_os = "linux", test))]
fn process_in_stat(pid: u32, stat: &str, page: u64, tick: u64) -> Option<Process> {
    let (before, after) = stat.rsplit_once(')')?;
    let (_, name) = before.split_once('(')?;
    let fields: Vec<&str> = after.split_whitespace().collect();
    // The state, past the name, is the third.
    let field = |number: usize| -> Option<u64> { fields.get(number - 3)?.parse().ok() };
    Some(Process {
        pid,
        parent: u32::try_from(field(4)?).ok()?,
        start: field(22)?,
        bytes: field(24)? * page,
        cpu: (field(14)? + field(15)?) * tick,
        name: name.to_string(),
    })
}

/// Every process on the machine, from libproc: its parent and its name for
/// any, and what it takes for this user's, which is all another user's
/// don't say. Not `ps`, which takes thirty times as long, a few hundred
/// processes read in a millisecond or two this way, and only says a
/// process's resident set.
#[cfg(target_os = "macos")]
fn processes() -> Vec<Process> {
    let timebase = mac::timebase();
    let pids = mac::pids();
    pids.into_iter()
        .filter_map(|pid| mac::process(pid, timebase))
        .collect()
}

/// Elsewhere crystal doesn't look.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn processes() -> Vec<Process> {
    Vec::new()
}

#[cfg(target_os = "macos")]
mod mac {
    use super::Process;
    use libc::{c_int, pid_t};
    use std::mem::{size_of, zeroed};

    #[repr(C)]
    pub struct Timebase {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> c_int;
    }

    /// What a tick of the kernel's clock is, in nanoseconds, as a fraction:
    /// a process's CPU time is kept in its ticks, 125/3 of a nanosecond
    /// each on Apple silicon, one on Intel.
    pub fn timebase() -> (u64, u64) {
        let mut base = Timebase { numer: 1, denom: 1 };
        // SAFETY: it fills in the struct it's given.
        let failed = unsafe { mach_timebase_info(&raw mut base) } != 0;
        match failed || base.denom == 0 {
            true => (1, 1),
            false => (base.numer.into(), base.denom.into()),
        }
    }

    /// Every process's pid.
    pub fn pids() -> Vec<u32> {
        // SAFETY: with no buffer, it says how many there are.
        let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
        let Ok(count) = usize::try_from(count) else {
            return Vec::new();
        };
        // With room for those started meanwhile.
        let mut pids: Vec<pid_t> = vec![0; count + 64];
        let room = c_int::try_from(pids.len() * size_of::<pid_t>()).unwrap_or(c_int::MAX);
        // SAFETY: the buffer has `room` bytes.
        let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), room) };
        pids.truncate(usize::try_from(listed).unwrap_or(0));
        pids.into_iter()
            .filter_map(|pid| u32::try_from(pid).ok())
            .collect()
    }

    /// The process `pid`, or `None` once it has gone. Another user's says
    /// only its parent and name.
    pub fn process(pid: u32, (numer, denom): (u64, u64)) -> Option<Process> {
        let id = pid_t::try_from(pid).ok()?;
        // SAFETY: both are plain structs of numbers, which zeroes make.
        let (mut info, mut usage): (libc::proc_bsdshortinfo, libc::rusage_info_v2) =
            unsafe { (zeroed(), zeroed()) };
        let size = c_int::try_from(size_of::<libc::proc_bsdshortinfo>()).ok()?;
        // SAFETY: the buffer is a proc_bsdshortinfo, `size` bytes.
        let read = unsafe {
            libc::proc_pidinfo(
                id,
                libc::PROC_PIDT_SHORTBSDINFO,
                0,
                (&raw mut info).cast(),
                size,
            )
        };
        if read != size {
            return None;
        }
        // SAFETY: the buffer is the rusage_info_v2 its flavor asks for.
        let used =
            unsafe { libc::proc_pid_rusage(id, libc::RUSAGE_INFO_V2, (&raw mut usage).cast()) }
                == 0;
        let name = info.pbsi_comm.iter().take_while(|&&byte| byte != 0);
        let name: Vec<u8> = name.map(|&byte| byte as u8).collect();
        let ticks = u128::from(usage.ri_user_time) + u128::from(usage.ri_system_time);
        let cpu = ticks * u128::from(numer) / u128::from(denom);
        Some(Process {
            pid,
            parent: info.pbsi_ppid,
            start: usage.ri_proc_start_abstime,
            bytes: if used { usage.ri_phys_footprint } else { 0 },
            cpu: if used {
                u64::try_from(cpu).unwrap_or(u64::MAX)
            } else {
                0
            },
            name: String::from_utf8_lossy(&name).into_owned(),
        })
    }
}

/// What runs under a process: whether a session left alone may be stopped
/// goes by it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Under {
    /// Some process runs under it.
    pub anything: bool,
    /// A process under it leads a session of its own: something called
    /// `setsid` to cut it loose from the terminal, the way Claude Code runs
    /// a Bash call in the background or a Monitor watch, and Codex a shell
    /// command, work that goes on after the turn that started it. The
    /// helpers an agent keeps in its own session, its MCP servers and the
    /// like, aren't. Adapted from docket's.
    pub detached_job: bool,
}

/// A process as [`under`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Member {
    pid: u32,
    parent: u32,
    leads_session: bool,
}

/// Every process on the machine, read once, to say what runs under each of
/// several.
pub struct Processes(Vec<Member>);

impl Processes {
    /// Reads them, or `None` when they can't be read, which is never to be
    /// taken for none: it reads every process on the machine, or runs `ps`,
    /// so it's not for an event loop.
    pub fn read() -> Option<Processes> {
        members().map(Processes)
    }

    /// What runs under the process `root`.
    pub fn under(&self, root: u32) -> Under {
        under_in(&self.0, root)
    }
}

fn under_in(members: &[Member], root: u32) -> Under {
    let mut children: HashMap<u32, Vec<&Member>> = HashMap::new();
    for member in members {
        children.entry(member.parent).or_default().push(member);
    }
    let mut found = Under::default();
    let mut seen = HashSet::new();
    let mut left = vec![root];
    while let Some(pid) = left.pop() {
        if !seen.insert(pid) {
            continue;
        }
        for child in children.get(&pid).into_iter().flatten() {
            found.anything = true;
            found.detached_job |= child.leads_session;
            left.push(child.pid);
        }
    }
    found
}

/// Every process on the machine, from `/proc`, with whether it leads its
/// session: its session's id is its own.
#[cfg(target_os = "linux")]
fn members() -> Option<Vec<Member>> {
    let entries = std::fs::read_dir("/proc").ok()?;
    let members = entries
        .flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            member_in_stat(pid, &stat)
        })
        .collect();
    Some(members)
}

/// A process as its `/proc/<pid>/stat` line says, past the program's name
/// in brackets: its state, its parent, its group, then its session. One
/// that has ended, a zombie its parent hasn't waited for yet, runs nothing
/// and holds nothing: `None`, as a job killed under an agent that reaps
/// late would otherwise hold it for good.
#[cfg(any(target_os = "linux", test))]
fn member_in_stat(pid: u32, stat: &str) -> Option<Member> {
    let (_, after) = stat.rsplit_once(')')?;
    let mut fields = after.split_whitespace();
    if ended(fields.next()?) {
        return None;
    }
    let parent = fields.next()?.parse().ok()?;
    let session: u32 = fields.nth(1)?.parse().ok()?;
    Some(Member {
        pid,
        parent,
        leads_session: session == pid,
    })
}

/// Every process on the machine, from `ps`, with whether it leads its
/// session: an `s` in its state.
#[cfg(not(target_os = "linux"))]
fn members() -> Option<Vec<Member>> {
    let listed = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,stat="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    Some(members_from_ps(&String::from_utf8_lossy(&listed.stdout)))
}

/// What `ps -axo pid=,ppid=,stat=` printed, a process a line. A line that
/// doesn't read is passed over, and so is a process that has ended, as
/// [`member_in_stat`] passes it over.
#[cfg(any(not(target_os = "linux"), test))]
fn members_from_ps(listed: &str) -> Vec<Member> {
    listed
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let parent = fields.next()?.parse().ok()?;
            let stat = fields.next().unwrap_or_default();
            if ended(stat) {
                return None;
            }
            let leads_session = stat.contains('s');
            Some(Member {
                pid,
                parent,
                leads_session,
            })
        })
        .collect()
}

/// Whether a process in the state `state`, as `/proc` or `ps` say it, has
/// ended: a zombie, or dead.
fn ended(state: &str) -> bool {
    state.starts_with(['Z', 'X', 'x'])
}

/// How big a page of memory is, in bytes.
fn page_bytes() -> u64 {
    // SAFETY: sysconf only reads a value of the system's.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(page).unwrap_or(4096)
}

/// The memory the machine has and its cores, or 0 for what it doesn't say.
fn machine() -> Machine {
    // SAFETY: sysconf only reads values of the system's.
    let (pages, cores) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_NPROCESSORS_ONLN),
        )
    };
    Machine {
        bytes: u64::try_from(pages).map_or(0, |pages| pages * page_bytes()),
        cores: u32::try_from(cores).unwrap_or(0),
    }
}

/// An amount of memory as a person reads it: `512 KB`, `412 MB`, `1.2 GB`.
pub fn size(bytes: u64) -> String {
    let (amount, unit) = amount(bytes);
    format!("{amount} {unit}B")
}

/// The same, short, where room is scarce: `512K`, `412M`, `1.2G`.
pub fn short_size(bytes: u64) -> String {
    let (amount, unit) = amount(bytes);
    format!("{amount}{unit}")
}

fn amount(bytes: u64) -> (String, char) {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    match bytes {
        0..MB => (bytes.div_ceil(KB).to_string(), 'K'),
        MB..GB => (((bytes + MB / 2) / MB).to_string(), 'M'),
        _ => (format!("{:.1}", bytes as f64 / GB as f64), 'G'),
    }
}

/// CPU in percent of one core as a person reads it: a tenth below ten,
/// `4.5%`, and whole above, `35%` or `250%`.
pub fn cpu(percent: f64) -> String {
    match percent {
        0.05..9.95 => format!("{percent:.1}%"),
        _ => format!("{percent:.0}%"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, pid: u32) -> (String, u32) {
        (name.to_string(), pid)
    }

    /// Processes a line each: the pid, the parent's, kilobytes and
    /// milliseconds of CPU, and when it started, 1 left out.
    fn listed(lines: &str) -> Vec<Process> {
        lines
            .lines()
            .filter_map(|line| {
                let fields: Vec<u64> = (line.split_whitespace())
                    .map(|field| field.parse().ok())
                    .collect::<Option<_>>()?;
                let [pid, parent, kilobytes, millis, ref rest @ ..] = fields[..] else {
                    return None;
                };
                Some(Process {
                    pid: pid as u32,
                    parent: parent as u32,
                    start: rest.first().copied().unwrap_or(1),
                    bytes: kilobytes * 1024,
                    cpu: millis * 1_000_000,
                    name: format!("p{pid}"),
                })
            })
            .collect()
    }

    fn whose<'a>(
        sessions: &'a [(String, u32)],
        client: Option<u32>,
        warm: Option<u32>,
    ) -> Whose<'a> {
        Whose {
            daemon: 10,
            client,
            sessions,
            warm,
        }
    }

    const MACHINE: Machine = Machine {
        bytes: 8 << 30,
        cores: 8,
    };

    #[test]
    fn a_session_takes_what_its_whole_tree_of_processes_takes() {
        // The daemon is 10, its sessions' programs 20 and 30; 20 runs 21
        // and 22, and 22 runs 23. 99 is nobody's, and 40 is the client.
        let listed = listed(
            " 10 1 1000 0\n 20 10 2000 0\n 21 20 300 0\n 22 20 500 0\n 23 22 200 0\n \
             30 10 50 0\n 99 1 9999 0\n 40 7 400 0\n",
        );
        let sessions = [named("agent", 20), named("shell", 30)];
        let taken = of(&listed, None, &whose(&sessions, Some(40), None), MACHINE);
        assert_eq!(taken.daemon.bytes, 1000 * 1024);
        assert_eq!(taken.client.unwrap().bytes, 400 * 1024);
        let agent = taken.sessions[0].usage;
        assert_eq!((agent.bytes, agent.processes), ((3000) * 1024, 4));
        let shell = taken.sessions[1].usage;
        assert_eq!((shell.bytes, shell.processes), (50 * 1024, 1));
        assert!(taken.helpers.is_empty());
        assert_eq!(taken.own().bytes, 1400 * 1024);
        assert_eq!(taken.all().bytes, (1400 + 3050) * 1024);
        assert_eq!(taken.all().processes, 7);
        assert_eq!(taken.machine, MACHINE);
        assert_eq!(taken.cpu_over_ms, 0);
    }

    #[test]
    fn what_the_daemon_runs_beside_its_sessions_is_its_helpers() {
        // 20 is a session's, 50 the agent kept warm; 60 is the distiller,
        // with 61 under it, and 70 git, which the client 40 runs one of
        // too, 41.
        let listed = listed(
            " 10 1 1000 0\n 20 10 2000 0\n 50 10 700 0\n 51 50 100 0\n 60 10 300 0\n \
             61 60 30 0\n 70 10 10 0\n 40 7 400 0\n 41 40 20 0\n",
        );
        let sessions = [named("agent", 20)];
        let taken = of(
            &listed,
            None,
            &whose(&sessions, Some(40), Some(50)),
            MACHINE,
        );
        let helpers: Vec<(&str, u64, u32)> = (taken.helpers.iter())
            .map(|helper| {
                (
                    helper.name.as_str(),
                    helper.usage.bytes,
                    helper.usage.processes,
                )
            })
            .collect();
        assert_eq!(helpers, [("p60", 330 * 1024, 2), ("p70", 10 * 1024, 1)]);
        assert_eq!(taken.warm.unwrap().bytes, 800 * 1024);
        assert_eq!(taken.client.unwrap().processes, 2);
        // Crystal itself: the daemon, its helpers and the client.
        assert_eq!(taken.own().bytes, (1000 + 340 + 420) * 1024);
        assert_eq!(taken.all().bytes, (1760 + 800 + 2000) * 1024);
    }

    #[test]
    fn a_process_that_has_gone_takes_nothing_and_a_cycle_counts_once() {
        let listed = listed(" 20 21 100 0\n 21 20 50 0\nnot a process\n");
        let sessions = [named("a", 20), named("b", 5)];
        let taken = of(
            &listed,
            None,
            &whose(&sessions, None, None),
            Machine::default(),
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
    fn cpu_is_the_time_each_process_had_since_the_look_before() {
        let mut earlier = Earlier::default();
        let start = Instant::now();
        let first = listed(" 10 1 1000 5000\n 20 10 2000 9000\n 21 20 100 300\n 30 10 50 100\n");
        assert!(
            earlier.count(start, &first).is_none(),
            "nothing to count from"
        );
        // Two seconds on: the daemon had half a second, the agent two and
        // its worker one, 30 has gone, and 22 started since, with 400 ms.
        let second = listed(" 10 1 1000 5500\n 20 10 2000 11000\n 21 20 100 1300\n 22 20 10 400\n");
        let spent = earlier.count(start + Duration::from_secs(2), &second);
        let sessions = [named("agent", 20), named("gone", 30)];
        let taken = of(
            &second,
            spent.as_ref(),
            &whose(&sessions, None, None),
            MACHINE,
        );
        assert_eq!(taken.cpu_over_ms, 2000);
        assert_eq!(taken.daemon.cpu, 25.0);
        // 2 s + 1 s + 0.4 s over 2 s: 170% of a core.
        assert_eq!(taken.sessions[0].usage.cpu, 170.0);
        assert_eq!(
            taken.sessions[1].usage,
            Usage {
                pid: 30,
                ..Usage::default()
            }
        );
        assert_eq!(taken.all().cpu, 195.0);
    }

    #[test]
    fn a_pid_given_to_another_process_counts_from_nothing() {
        let mut earlier = Earlier::default();
        let start = Instant::now();
        earlier.count(start, &listed(" 20 10 100 60000 7\n"));
        // 20 went, and a process started since has its pid, with 300 ms:
        // not a minute less than the one before had.
        let again = listed(" 20 10 100 300 8\n");
        let spent = earlier.count(start + Duration::from_secs(1), &again);
        let family = Family::of(&again, spent.as_ref());
        assert_eq!(family.alone(20).cpu, 30.0);
    }

    #[test]
    fn a_look_a_moment_after_another_counts_from_the_one_before_it() {
        let mut earlier = Earlier::default();
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let cpu = |millis| listed(&format!(" 10 1 100 {millis}\n"));
        assert!(earlier.count(at(0), &cpu(0)).is_none());
        // A moment on, there's still nothing to count from.
        assert!(earlier.count(at(100), &cpu(50)).is_none());
        let spent = earlier.count(at(1000), &cpu(500)).unwrap();
        assert_eq!(
            (spent.over, spent.by[&10]),
            (Duration::from_secs(1), 500_000_000)
        );
        // Another client asks a moment after: from the look before.
        let spent = earlier.count(at(1100), &cpu(600)).unwrap();
        assert_eq!(spent.over, Duration::from_millis(1100));
        // A while after, from the newest.
        let spent = earlier.count(at(2000), &cpu(1000)).unwrap();
        assert_eq!(
            (spent.over, spent.by[&10]),
            (Duration::from_secs(1), 500_000_000)
        );
        // Long after, nothing: the next counts from this one.
        assert!(earlier.count(at(60_000), &cpu(2000)).is_none());
        let spent = earlier.count(at(61_000), &cpu(2100)).unwrap();
        assert_eq!(spent.by[&10], 100_000_000);
    }

    #[test]
    fn a_process_is_read_past_a_name_with_brackets_and_spaces() {
        // As Linux writes it: CPU 150 and 50 ticks, started at tick 9000,
        // 300 pages resident.
        let stat = "123 (my (odd) prog) S 45 123 123 0 -1 4194560 100 0 0 0 150 50 0 0 20 0 1 0 \
                    9000 1000000 300 18446744073709551615";
        let process = process_in_stat(123, stat, 4096, 10_000_000).unwrap();
        assert_eq!(
            process,
            Process {
                pid: 123,
                parent: 45,
                start: 9000,
                bytes: 300 * 4096,
                cpu: 2_000_000_000,
                name: "my (odd) prog".to_string(),
            }
        );
        assert_eq!(process_in_stat(1, "garbage", 4096, 1), None);
        assert_eq!(process_in_stat(1, "1 (short) S 0 1", 4096, 1), None);
    }

    #[test]
    fn a_job_cut_loose_from_the_terminal_is_told_from_an_agents_own_helpers() {
        // The agent is 20, leading its terminal's session; its node worker
        // 21 and MCP server 22 are its own, and 23, under 22, leads a
        // session of its own: a Bash call run in the background.
        let listed = " 20  10 Ss+\n 21  20 S+\n 22  20 S\n 23  22 Ss\n 30  10 Ss+\n 40  30 S+\n";
        let members = members_from_ps(listed);
        let agent = under_in(&members, 20);
        assert_eq!(
            agent,
            Under {
                anything: true,
                detached_job: true
            }
        );
        // A shell running a command in front has something under it, but
        // no job of its own.
        assert_eq!(
            under_in(&members, 30),
            Under {
                anything: true,
                detached_job: false
            }
        );
        // Nothing under the command, and nothing under what has gone.
        assert_eq!(under_in(&members, 40), Under::default());
        assert_eq!(under_in(&members, 99), Under::default());
        // A loop of parents is looked at once.
        let looped = members_from_ps(" 50 51 S\n 51 50 S\nnot a process\n");
        assert!(under_in(&looped, 50).anything);
    }

    #[test]
    fn a_process_leads_its_session_when_the_session_is_its_own() {
        let stat = "23 (bash (bg)) S 22 23 23 0 -1 4194560 100";
        let member = member_in_stat(23, stat).unwrap();
        assert_eq!((member.parent, member.leads_session), (22, true));
        let stat = "21 (node) S 20 20 20 34816 20 4194560 100";
        assert!(!member_in_stat(21, stat).unwrap().leads_session);
        assert_eq!(member_in_stat(1, "garbage"), None);
    }

    #[test]
    fn a_job_that_has_ended_holds_nothing_before_it_is_reaped() {
        // The job 23, killed, is a zombie until the agent 20 waits for it:
        // its session is still its own.
        let stat = "23 (sleep) Z 20 23 23 0 -1 4227076 0";
        assert_eq!(member_in_stat(23, stat), None);
        let listed = " 20  10 Ss+
 23  20 Zs
 24  20 Z
";
        let members = members_from_ps(listed);
        assert_eq!(under_in(&members, 20), Under::default());
    }

    #[test]
    fn this_process_has_a_child_while_one_runs() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let found = Processes::read().unwrap().under(std::process::id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(found.anything);
    }

    #[test]
    fn sizes_and_cpu_read_as_a_person_says_them() {
        assert_eq!(size(500), "1 KB");
        assert_eq!(size(412 * 1024 * 1024 + 1), "412 MB");
        assert_eq!(size(1288 * 1024 * 1024), "1.3 GB");
        assert_eq!(short_size(2254 * 1024 * 1024), "2.2G");
        assert_eq!(short_size(412 * 1024 * 1024), "412M");
        assert_eq!(cpu(0.0), "0%");
        assert_eq!(cpu(0.04), "0%");
        assert_eq!(cpu(4.5), "4.5%");
        assert_eq!(cpu(9.96), "10%");
        assert_eq!(cpu(250.0), "250%");
    }

    #[test]
    fn the_machine_has_memory_and_cores_and_crystal_takes_some() {
        let machine = machine();
        assert!(machine.bytes > 0 && machine.cores > 0);
        // A thread of this process keeps a core busy while it's measured,
        // with nothing to count from: it looks twice, half a second apart.
        let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let spinning = thread::spawn({
            let busy = busy.clone();
            move || {
                while busy.load(std::sync::atomic::Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            }
        });
        let me = std::process::id();
        let sessions = [named("me", me)];
        let whose = Whose {
            daemon: me,
            client: None,
            sessions: &sessions,
            warm: Some(me),
        };
        let taken = measure(&Mutex::default(), &whose);
        busy.store(false, std::sync::atomic::Ordering::Relaxed);
        spinning.join().unwrap();
        assert_eq!(taken.warm.unwrap().pid, me);
        assert!(taken.daemon.bytes > 0);
        assert!(taken.sessions[0].usage.processes >= 1);
        assert!(taken.cpu_over_ms >= 500, "{}", taken.cpu_over_ms);
        assert!(taken.daemon.cpu > 5.0, "{}", taken.daemon.cpu);
    }
}
